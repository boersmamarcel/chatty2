//! Shared SSRF (server-side request forgery) host validation.
//!
//! `fetch_tool` (agent-chosen URLs on the open internet) and the browser's
//! internet-enabled navigation policy both need to refuse the same private,
//! loopback, link-local, and cloud-metadata targets. One denylist here, so
//! the two can't drift apart the way two hand-copied lists would.

use std::net::IpAddr;
use tracing::warn;

/// Validate that `url`'s host is not a private, internal, loopback, or
/// cloud-metadata target. Resolves hostnames to catch DNS rebinding /
/// split-horizon attacks where a public name resolves to a private IP.
pub fn check_public_host(url: &str) -> Result<(), String> {
    check_public_host_impl(url, false)
}

/// Same as [`check_public_host`], except when `allow_private_network_access`
/// is true, private/internal ranges (RFC-1918, loopback-as-IP, CGN,
/// benchmarking, multicast, "this network") are not rejected.
///
/// The link-local range `169.254.0.0/16` — which includes the cloud-metadata
/// address `169.254.169.254` — is never bypassed, even with the flag on:
/// that carve-out is deliberate (AGE-459) and applied before the flag is
/// consulted at all, in [`is_blocked_ip`]. Only the browser's per-workspace
/// toggle should ever pass `true` here; `fetch_tool` always calls
/// [`check_public_host`].
pub fn check_public_host_with_bypass(
    url: &str,
    allow_private_network_access: bool,
) -> Result<(), String> {
    check_public_host_impl(url, allow_private_network_access)
}

fn check_public_host_impl(url: &str, allow_private_network_access: bool) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("invalid URL {url}: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| format!("{url} has no host"))?;

    if is_blocked_hostname(host) {
        return Err(format!(
            "requests to '{host}' are blocked for security (SSRF protection)"
        ));
    }

    // Strip brackets for IPv6: host_str() returns "[::1]" but IpAddr expects "::1".
    let ip_str = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = ip_str.parse::<IpAddr>() {
        if is_blocked_ip(&ip, allow_private_network_access) {
            return Err(format!(
                "requests to private/internal IP '{ip}' are blocked for security (SSRF protection)"
            ));
        }
        return Ok(());
    }

    // Hostname: resolve and check the resolved address too.
    if let Ok(addrs) = std::net::ToSocketAddrs::to_socket_addrs(&(host, 80)) {
        for addr in addrs {
            if is_blocked_ip(&addr.ip(), allow_private_network_access) {
                warn!(host = %host, resolved_ip = %addr.ip(), "Blocked DNS-resolved private IP");
                return Err(format!(
                    "'{host}' resolves to private/internal IP {} (SSRF protection)",
                    addr.ip()
                ));
            }
        }
    }

    Ok(())
}

/// Whether `ip` should be rejected, given whether the caller has opted into
/// reaching the rest of the private-network space.
///
/// Link-local (including cloud metadata) is checked first and always blocks,
/// regardless of `allow_private_network_access` — see [`check_public_host_with_bypass`].
fn is_blocked_ip(ip: &IpAddr, allow_private_network_access: bool) -> bool {
    if is_link_local_ip(ip) {
        return true;
    }
    if allow_private_network_access {
        return false;
    }
    is_private_ip(ip)
}

/// Check if a hostname string is a known-blocked name (case-insensitive).
pub fn is_blocked_hostname(host: &str) -> bool {
    let h = host.to_lowercase();
    h == "localhost"
        || h == "metadata.google.internal"  // GCP metadata
        || h.ends_with(".internal")
        || h.ends_with(".local")
}

/// Check if an IP address is in the link-local range: `169.254.0.0/16` for
/// IPv4 (which includes the AWS/GCP/Azure cloud-metadata address
/// `169.254.169.254`) and `fe80::/10` for IPv6, including either reached
/// through an IPv4-mapped IPv6 address. Kept as its own predicate, separate
/// from [`is_private_ip`], because AGE-459's workspace-level toggle carves
/// this range out explicitly rather than lumping it in with "private".
pub fn is_link_local_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            octets[0] == 169 && octets[1] == 254
        }
        IpAddr::V6(v6) => {
            (v6.segments()[0] & 0xffc0) == 0xfe80
                || v6
                    .to_ipv4_mapped()
                    .map(|v4| is_link_local_ip(&IpAddr::V4(v4)))
                    .unwrap_or(false)
        }
    }
}

/// Check if an IP address belongs to a private, loopback, link-local, or otherwise
/// reserved network range that should not be reachable from an open-web request.
pub fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            // 127.0.0.0/8 — loopback
            octets[0] == 127
            // 10.0.0.0/8 — RFC-1918 private
            || octets[0] == 10
            // 172.16.0.0/12 — RFC-1918 private
            || (octets[0] == 172 && (16..=31).contains(&octets[1]))
            // 192.168.0.0/16 — RFC-1918 private
            || (octets[0] == 192 && octets[1] == 168)
            // 169.254.0.0/16 — link-local (includes AWS/GCP/Azure metadata at 169.254.169.254)
            || (octets[0] == 169 && octets[1] == 254)
            // 0.0.0.0/8 — "this" network
            || octets[0] == 0
            // 100.64.0.0/10 — shared address space (CGN, often internal)
            || (octets[0] == 100 && (64..=127).contains(&octets[1]))
            // 198.18.0.0/15 — benchmarking
            || (octets[0] == 198 && (18..=19).contains(&octets[1]))
            // 224.0.0.0/4 — multicast
            || octets[0] >= 224
        }
        IpAddr::V6(v6) => {
            // ::1 — loopback
            v6.is_loopback()
            // fe80::/10 — link-local
            || (v6.segments()[0] & 0xffc0) == 0xfe80
            // fc00::/7 — unique local (ULA, RFC-4193)
            || (v6.segments()[0] & 0xfe00) == 0xfc00
            // :: — unspecified
            || v6.is_unspecified()
            // ::ffff:x.x.x.x — IPv4-mapped, check the embedded v4 address
            || v6.to_ipv4_mapped().map(|v4| is_private_ip(&IpAddr::V4(v4))).unwrap_or(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_private_ip_loopback() {
        assert!(is_private_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_private_ip(&"127.0.0.2".parse().unwrap()));
        assert!(is_private_ip(&"::1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_rfc1918() {
        assert!(is_private_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_private_ip(&"10.255.255.255".parse().unwrap()));
        assert!(is_private_ip(&"172.16.0.1".parse().unwrap()));
        assert!(is_private_ip(&"172.31.255.255".parse().unwrap()));
        assert!(is_private_ip(&"192.168.0.1".parse().unwrap()));
        assert!(is_private_ip(&"192.168.255.255".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_link_local_and_metadata() {
        // AWS/GCP/Azure metadata endpoint
        assert!(is_private_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_private_ip(&"169.254.0.1".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_public() {
        assert!(!is_private_ip(&"8.8.8.8".parse().unwrap()));
        assert!(!is_private_ip(&"1.1.1.1".parse().unwrap()));
        assert!(!is_private_ip(&"93.184.216.34".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_other_reserved() {
        assert!(is_private_ip(&"0.0.0.0".parse().unwrap()));
        assert!(is_private_ip(&"100.64.0.1".parse().unwrap())); // CGN
        assert!(is_private_ip(&"224.0.0.1".parse().unwrap())); // multicast
        assert!(is_private_ip(&"255.255.255.255".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_v6() {
        // ULA
        assert!(is_private_ip(&"fd00::1".parse().unwrap()));
        // Link-local
        assert!(is_private_ip(&"fe80::1".parse().unwrap()));
        // Unspecified
        assert!(is_private_ip(&"::".parse().unwrap()));
    }

    #[test]
    fn is_private_ip_v4_mapped_v6() {
        // ::ffff:127.0.0.1 should be blocked
        assert!(is_private_ip(&"::ffff:127.0.0.1".parse().unwrap()));
        // ::ffff:169.254.169.254 (metadata via v6)
        assert!(is_private_ip(&"::ffff:169.254.169.254".parse().unwrap()));
        // ::ffff:8.8.8.8 should be allowed
        assert!(!is_private_ip(&"::ffff:8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn blocked_hostname() {
        assert!(is_blocked_hostname("localhost"));
        assert!(is_blocked_hostname("LOCALHOST"));
        assert!(is_blocked_hostname("metadata.google.internal"));
        assert!(is_blocked_hostname("foo.internal"));
        assert!(is_blocked_hostname("printer.local"));
        assert!(!is_blocked_hostname("example.com"));
        assert!(!is_blocked_hostname("my-internal-api.com")); // "internal" in domain name is fine
    }

    #[test]
    fn check_public_host_blocks_private() {
        assert!(check_public_host("http://127.0.0.1/secret").is_err());
        assert!(check_public_host("http://localhost:8080/admin").is_err());
        assert!(check_public_host("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(check_public_host("http://10.0.0.1/internal").is_err());
        assert!(check_public_host("http://192.168.1.1/router").is_err());
        assert!(check_public_host("http://172.16.0.5/service").is_err());
        assert!(check_public_host("http://[::1]/secret").is_err());
        assert!(check_public_host("http://metadata.google.internal/computeMetadata/v1/").is_err());
    }

    #[test]
    fn check_public_host_allows_public() {
        assert!(check_public_host("https://example.com").is_ok());
        assert!(check_public_host("https://docs.rs/rig-core/latest").is_ok());
    }

    #[test]
    fn is_link_local_ip_covers_metadata_range() {
        assert!(is_link_local_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_link_local_ip(&"169.254.0.1".parse().unwrap()));
        assert!(is_link_local_ip(&"fe80::1".parse().unwrap()));
        assert!(is_link_local_ip(&"::ffff:169.254.169.254".parse().unwrap()));
        assert!(!is_link_local_ip(&"10.0.0.1".parse().unwrap()));
        assert!(!is_link_local_ip(&"192.168.1.1".parse().unwrap()));
        assert!(!is_link_local_ip(&"8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn check_public_host_with_bypass_off_matches_check_public_host() {
        // AGE-459: with the bypass off, behavior is unchanged from
        // `check_public_host` — every RFC-1918 / loopback-IP / metadata
        // target is still rejected.
        for url in [
            "http://10.0.0.1/internal",
            "http://192.168.1.1/router",
            "http://172.16.0.5/service",
            "http://169.254.169.254/latest/meta-data/",
        ] {
            assert!(check_public_host_with_bypass(url, false).is_err());
        }
    }

    #[test]
    fn check_public_host_with_bypass_on_allows_private_ranges() {
        for url in [
            "http://10.0.0.1/internal",
            "http://192.168.1.1/router",
            "http://172.16.0.5/service",
            "http://100.64.0.1/cgn",
        ] {
            assert!(
                check_public_host_with_bypass(url, true).is_ok(),
                "{url} should be reachable with the toggle on"
            );
        }
    }

    #[test]
    fn check_public_host_with_bypass_on_still_blocks_link_local_metadata() {
        // AGE-459: the toggle never reaches the cloud-metadata range,
        // carved out ahead of the bypass check.
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.1.1/",
            "http://[fe80::1]/",
        ] {
            assert!(
                check_public_host_with_bypass(url, true).is_err(),
                "{url} must stay blocked even with the toggle on"
            );
        }
    }

    #[test]
    fn check_public_host_with_bypass_on_still_blocks_hostnames() {
        // The hostname denylist (localhost, .internal, .local, the GCP
        // metadata hostname) is a separate mechanism from the private-IP
        // check and is not affected by the toggle.
        for url in [
            "http://localhost:8080/admin",
            "http://foo.internal/",
            "http://printer.local/",
            "http://metadata.google.internal/computeMetadata/v1/",
        ] {
            assert!(check_public_host_with_bypass(url, true).is_err());
        }
    }

    #[test]
    fn check_public_host_with_bypass_on_still_allows_public_hosts() {
        assert!(check_public_host_with_bypass("https://example.com", true).is_ok());
    }
}
