//! What one worker connection may cost the broker (EN-0b, ADR-0021 step 0).
//!
//! A worker is a same-uid process the broker does not trust to be well
//! behaved: a buggy or compromised one must not be able to exhaust the
//! broker's memory. So every connection reads lines of at most
//! [`MAX_FRAME_BYTES`], queues at most [`OUTBOUND_QUEUE_FRAMES`] frames for
//! its writer, and runs at most [`MAX_IN_FLIGHT_CALLS`] calls at a time,
//! started at no more than [`CALLS_PER_SECOND`] (with a burst of
//! [`CALL_BURST`]), and keeps at most [`MAX_PENDING_APPROVALS`]
//! `human.approve` requests waiting on the root.
//!
//! What happens at each limit follows one rule: only a peer's own frame
//! closes its connection.
//! - A line over the cap, or one that does not decode, closes the
//!   connection it arrived on.
//! - Relayed content that would put a frame over the *receiver's* cap fails
//!   only the request it answers.
//! - A full outbound queue makes whoever is queueing wait; the receiving
//!   connection is never closed for being slow.
//! - A call over the in-flight or rate cap is refused like any other
//!   refused call; an approval over its cap is denied without asking.

// The socket that reads lines and counts calls is Unix-only (`mod.rs`).
#![cfg_attr(not(unix), allow(dead_code))]

use std::io;
use std::time::Duration;

use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::time::Instant;

/// The longest line either end of a worker connection accepts, in bytes,
/// newline excluded: RC-0's 32 MiB conversation capture plus framing.
///
/// Interim: ADR-0021 Q4 will set per-direction caps.
pub const MAX_FRAME_BYTES: usize = 33 * 1024 * 1024;

/// Frames queued for one connection's writer before a producer waits.
pub const OUTBOUND_QUEUE_FRAMES: usize = 64;

/// Calls one connection may have running at once.
pub const MAX_IN_FLIGHT_CALLS: usize = 32;

/// `human.approve` requests one connection may have waiting on the root at
/// once (EN-2a). A worker's turn waits on one approval at a time; the rest
/// is headroom for parallel tool calls, not a queue to fill the root's
/// screen with.
pub const MAX_PENDING_APPROVALS: usize = 4;

/// Calls one connection may start per second, sustained.
pub const CALLS_PER_SECOND: u32 = 10;

/// Calls one connection may start back to back before [`CALLS_PER_SECOND`]
/// applies.
pub const CALL_BURST: u32 = MAX_IN_FLIGHT_CALLS as u32;

/// Newline-delimited lines from `R`, each at most `max` bytes.
///
/// Unlike [`tokio::io::Lines`], it never buffers more than `max` bytes of
/// one line: a longer line is an [`io::ErrorKind::InvalidData`] error as
/// soon as it passes the cap, before the rest of it is read. So is a line
/// that is not UTF-8. A trailing `\r` is stripped, as `Lines` does.
pub struct BoundedLines<R> {
    reader: R,
    buf: Vec<u8>,
    max: usize,
}

impl<R: AsyncBufRead + Unpin> BoundedLines<R> {
    pub fn new(reader: R, max: usize) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            max,
        }
    }

    /// The next line, `None` at the end of the stream. After an error the
    /// stream is not worth reading further.
    pub async fn next_line(&mut self) -> io::Result<Option<String>> {
        self.buf.clear();
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                break;
            }
            let (chunk, used, done) = match available.iter().position(|b| *b == b'\n') {
                Some(at) => (&available[..at], at + 1, true),
                None => (available, available.len(), false),
            };
            if self.buf.len() + chunk.len() > self.max {
                return Err(oversized(self.max));
            }
            self.buf.extend_from_slice(chunk);
            self.reader.consume(used);
            if done {
                break;
            }
        }
        if self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }
        String::from_utf8(std::mem::take(&mut self.buf))
            .map(Some)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a line is not UTF-8"))
    }
}

fn oversized(max: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("a line is longer than the {max}-byte frame cap"),
    )
}

/// A token bucket: `burst` tokens, refilled at `per_second`.
#[derive(Debug)]
pub struct RateLimit {
    tokens: f64,
    burst: f64,
    per_second: f64,
    last: Instant,
}

impl RateLimit {
    pub fn new(per_second: u32, burst: u32, now: Instant) -> Self {
        Self {
            tokens: f64::from(burst),
            burst: f64::from(burst),
            per_second: f64::from(per_second),
            last: now,
        }
    }

    /// Take one token at `now`; `false` when there is none left.
    pub fn try_take(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last);
        self.last = now;
        self.tokens = (self.tokens + elapsed.as_secs_f64() * self.per_second).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// How long until the next token, at the configured rate.
    pub fn period(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.per_second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn lines(input: &[u8], max: usize) -> Vec<io::Result<Option<String>>> {
        // A tiny buffer, so a line spans many reads.
        let reader = tokio::io::BufReader::with_capacity(3, input);
        let mut lines = BoundedLines::new(reader, max);
        let mut out = Vec::new();
        loop {
            let next = lines.next_line().await;
            let end = !matches!(next, Ok(Some(_)));
            out.push(next);
            if end {
                return out;
            }
        }
    }

    #[tokio::test]
    async fn lines_up_to_the_cap_are_read_whole() {
        let out = lines(b"abcd\nab\r\n\nlast", 4).await;
        let got: Vec<_> = out.into_iter().map(|line| line.unwrap()).collect();
        assert_eq!(
            got,
            [
                Some("abcd".to_string()),
                Some("ab".to_string()),
                Some(String::new()),
                Some("last".to_string()),
                None
            ]
        );
    }

    #[tokio::test]
    async fn a_line_over_the_cap_is_an_error() {
        let out = lines(b"ok\nabcde\nnever", 4).await;
        assert_eq!(out[0].as_ref().unwrap().as_deref(), Some("ok"));
        let error = out[1].as_ref().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn a_line_that_is_not_utf8_is_an_error() {
        let out = lines(b"\xff\xfe\n", 4).await;
        assert_eq!(
            out[0].as_ref().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn the_rate_limit_refills_at_its_rate() {
        let start = Instant::now();
        let mut limit = RateLimit::new(10, 3, start);
        assert!((0..3).all(|_| limit.try_take(start)), "the burst");
        assert!(!limit.try_take(start), "then nothing");
        assert!(limit.try_take(start + limit.period()), "one per period");
        assert!(!limit.try_take(start + limit.period()));
        let later = start + Duration::from_secs(60);
        assert_eq!((0..10).filter(|_| limit.try_take(later)).count(), 3);
    }
}
