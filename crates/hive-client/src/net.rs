//! Shared network-safety primitives (AGE-1325): the capped body reader and
//! the loopback host classifier.
//!
//! These are security boundaries (response-size DoS caps; SSRF and
//! loopback policy) that used to be hand-copied at several call sites, so a
//! fix to one copy never reached the others. They live here because
//! `hive-client` is the lowest crate every consumer (chatty-core, the
//! protocol gateway, hive-client itself) already depends on, and it already
//! hosts the shared TLS gate [`crate::ensure_secure_url`].
//!
//! The classifiers answer one question only: "is this host loopback?". Each
//! call site layers its own policy (allow loopback, deny private ranges,
//! allow the LAN) on top, so a site keeps its own strictness.

use std::net::IpAddr;

use futures_util::{Stream, StreamExt as _};

/// What [`read_capped`] does when the body would pass the cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    /// Fail with [`CappedReadError::Overflow`]; no partial body is returned.
    Error,
    /// Keep exactly `cap` bytes, stop reading, and report `truncated`.
    Truncate,
}

/// Why [`read_capped`] failed.
#[derive(Debug, PartialEq, Eq)]
pub enum CappedReadError<E> {
    /// The body went past the cap under [`Overflow::Error`].
    Overflow,
    /// The stream itself failed.
    Stream(E),
}

/// A body read by [`read_capped`].
#[derive(Debug, PartialEq, Eq)]
pub struct CappedBody {
    pub bytes: Vec<u8>,
    /// The body went on past the cap and the rest was not read. Only ever
    /// true under [`Overflow::Truncate`].
    pub truncated: bool,
}

/// Read a chunked body, never holding more than `cap` bytes.
///
/// A body of exactly `cap` bytes is complete under both modes; one more byte
/// is an overflow. On [`Overflow::Error`] the offending chunk is not
/// buffered; on [`Overflow::Truncate`] the chunk is cut at the cap and the
/// remaining stream is left unread. `on_progress` gets the buffered length
/// after every chunk that was (partly) kept.
pub async fn read_capped<S, B, E>(
    stream: S,
    cap: usize,
    overflow: Overflow,
    mut on_progress: impl FnMut(usize),
) -> Result<CappedBody, CappedReadError<E>>
where
    S: Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
{
    futures_util::pin_mut!(stream);
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(CappedReadError::Stream)?;
        let chunk = chunk.as_ref();
        let room = cap - bytes.len();
        if chunk.len() > room {
            return match overflow {
                Overflow::Error => Err(CappedReadError::Overflow),
                Overflow::Truncate => {
                    bytes.extend_from_slice(&chunk[..room]);
                    on_progress(bytes.len());
                    Ok(CappedBody {
                        bytes,
                        truncated: true,
                    })
                }
            };
        }
        bytes.extend_from_slice(chunk);
        on_progress(bytes.len());
    }
    Ok(CappedBody {
        bytes,
        truncated: false,
    })
}

/// `host` without the brackets an IPv6 literal carries in a URL
/// (`[::1]` -> `::1`). Each side is trimmed independently, as every call
/// site always did.
pub fn strip_ip_brackets(host: &str) -> &str {
    host.trim_start_matches('[').trim_end_matches(']')
}

/// Whether `host` names this machine by name: `localhost`, in any case. The
/// trailing-dot form (`localhost.`) is deliberately not accepted: the allow
/// sites never accepted it and this predicate must not widen them. Deny
/// sites that want it trim the dot first.
pub fn is_localhost_name(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
}

/// A `*.localhost` name (RFC 6761 reserves the TLD for loopback) with a
/// non-empty prefix and no trailing dot: the browser profile's Lane A rule.
pub fn is_localhost_subdomain(host: &str) -> bool {
    host.to_ascii_lowercase()
        .strip_suffix(".localhost")
        .is_some_and(|prefix| !prefix.is_empty())
}

/// `127.0.0.0/8` or `::1`. An IPv4-mapped IPv6 address is not loopback here;
/// see [`is_loopback_ip_or_mapped`].
pub fn is_loopback_ip(ip: &IpAddr) -> bool {
    ip.is_loopback()
}

/// [`is_loopback_ip`], also seeing through an IPv4-mapped IPv6 address
/// (`::ffff:127.0.0.1`).
pub fn is_loopback_ip_or_mapped(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// A bare host (no brackets, no port) that is `localhost` or a loopback IP
/// literal, with [`is_loopback_ip`] semantics (no IPv4-mapped IPv6). No DNS.
pub fn is_loopback_host(host: &str) -> bool {
    is_localhost_name(host) || host.parse::<IpAddr>().is_ok_and(|ip| is_loopback_ip(&ip))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;

    fn chunks(sizes: &[usize]) -> impl Stream<Item = Result<Vec<u8>, Infallible>> + use<> {
        futures_util::stream::iter(
            sizes
                .iter()
                .map(|n| Ok::<_, Infallible>(vec![b'x'; *n]))
                .collect::<Vec<_>>(),
        )
    }

    const CAP: usize = 10;
    // Total body lengths around the cap, each cut into uneven chunks.
    const SPLITS: [&[usize]; 3] = [&[4, 3, 2], &[5, 5], &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1]];

    #[tokio::test]
    async fn error_mode_at_the_cap_boundary() {
        for (total, ok) in [(CAP - 1, true), (CAP, true), (CAP + 1, false)] {
            for split in [
                chunks(&even_split(total, 3)),
                chunks(&even_split(total, 1)),
                chunks(&[total]),
            ] {
                let got = read_capped(split, CAP, Overflow::Error, |_| {}).await;
                if ok {
                    let body = got.expect("within the cap");
                    assert_eq!(body.bytes.len(), total);
                    assert!(!body.truncated);
                } else {
                    assert_eq!(got.unwrap_err(), CappedReadError::Overflow);
                }
            }
        }
    }

    #[tokio::test]
    async fn truncate_mode_at_the_cap_boundary() {
        for (total, cut) in [(CAP - 1, false), (CAP, false), (CAP + 1, true)] {
            for split in [
                chunks(&even_split(total, 3)),
                chunks(&even_split(total, 1)),
                chunks(&[total]),
            ] {
                let body = read_capped(split, CAP, Overflow::Truncate, |_| {})
                    .await
                    .unwrap();
                assert_eq!(body.bytes.len(), total.min(CAP));
                assert_eq!(body.truncated, cut, "total {total}");
            }
        }
    }

    #[tokio::test]
    async fn truncate_stops_reading_after_the_cap() {
        let mut polled = 0;
        let stream = futures_util::stream::iter(vec![Ok::<_, Infallible>(vec![0u8; 6]); 5])
            .inspect(|_| polled += 1);
        let body = read_capped(stream, CAP, Overflow::Truncate, |_| {})
            .await
            .unwrap();
        assert_eq!((body.bytes.len(), body.truncated), (CAP, true));
        assert_eq!(polled, 2, "the third chunk must never be pulled");
    }

    #[tokio::test]
    async fn progress_reports_the_buffered_length() {
        let mut seen = Vec::new();
        read_capped(chunks(SPLITS[0]), CAP, Overflow::Error, |n| seen.push(n))
            .await
            .unwrap();
        assert_eq!(seen, [4, 7, 9]);
    }

    #[tokio::test]
    async fn stream_errors_surface() {
        let stream = futures_util::stream::iter(vec![Ok(vec![1u8]), Err("boom")]);
        let got = read_capped(stream, CAP, Overflow::Truncate, |_| {}).await;
        assert_eq!(got.unwrap_err(), CappedReadError::Stream("boom"));
    }

    /// `total` bytes as chunks of at most `step` (3 -> 3,3,3,1).
    fn even_split(total: usize, step: usize) -> Vec<usize> {
        let step = if step == 1 { 1 } else { 3 };
        let mut left = total;
        let mut out = Vec::new();
        while left > 0 {
            let n = left.min(step);
            out.push(n);
            left -= n;
        }
        out
    }

    #[test]
    fn loopback_host_table() {
        // (host, is_loopback_host, is_loopback_ip_or_mapped(host as IP),
        //  is_localhost_subdomain)
        let table: [(&str, bool, bool, bool); 16] = [
            ("localhost", true, false, false),
            ("LOCALHOST", true, false, false),
            ("localhost.", false, false, false),
            ("foo.localhost", false, false, true),
            ("foo.localhost.", false, false, false),
            (".localhost", false, false, false),
            ("localhost.evil.example", false, false, false),
            ("127.0.0.1", true, true, false),
            ("127.1.2.3", true, true, false),
            ("::1", true, true, false),
            ("::ffff:127.0.0.1", false, true, false),
            ("0.0.0.0", false, false, false),
            ("::", false, false, false),
            ("10.0.0.1", false, false, false),
            ("[::1]", false, false, false),
            ("example.com", false, false, false),
        ];
        for (host, loop_host, ip_mapped, sub) in table {
            assert_eq!(is_loopback_host(host), loop_host, "is_loopback_host {host}");
            assert_eq!(
                host.parse::<IpAddr>()
                    .is_ok_and(|ip| is_loopback_ip_or_mapped(&ip)),
                ip_mapped,
                "or_mapped {host}"
            );
            assert_eq!(is_localhost_subdomain(host), sub, "subdomain {host}");
        }
        assert_eq!(strip_ip_brackets("[::1]"), "::1");
        assert!(is_loopback_host(strip_ip_brackets("[::1]")));
    }
}
