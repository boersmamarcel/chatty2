//! Only loopback callers: a guard against DNS rebinding (S3 row 3.13).
//!
//! Binding to 127.0.0.1 does not keep a browser out. A page on
//! `evil.example` can re-resolve its own name to 127.0.0.1 and is then
//! same-origin with this server, so it could call every module (whose
//! `llm::complete` spends the user's provider key) and every virtual agent
//! (which starts a local worker). What it cannot change is the `Host` its
//! requests carry, nor the `Origin` a cross-origin request carries, so a
//! request is refused with 403 when:
//!
//! * `Host` is present and is not a loopback name (`localhost`, `127.0.0.0/8`,
//!   `::1`), or
//! * `Origin` is present and is not a loopback origin (`null` included).
//!
//! A request with no `Host` at all (HTTP/1.0, an in-process `oneshot`) is
//! not a browser's, and is served. The MCP transport spec asks servers to
//! validate `Origin` for this reason.

use std::net::IpAddr;

use axum::{
    Json,
    extract::Request,
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// Middleware: refuse a request whose `Host` or `Origin` is not loopback.
pub(crate) async fn loopback_only(request: Request, next: Next) -> Response {
    match refusal(request.headers()) {
        None => next.run(request).await,
        Some(reason) => {
            tracing::warn!(reason, "refused a non-loopback request");
            (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": format!("forbidden: {reason}") })),
            )
                .into_response()
        }
    }
}

/// Why `headers` are refused, or `None` when they are loopback.
fn refusal(headers: &HeaderMap) -> Option<&'static str> {
    if let Some(host) = headers.get(header::HOST)
        && !host.to_str().is_ok_and(is_loopback_authority)
    {
        return Some("the Host header is not a loopback name");
    }
    if let Some(origin) = headers.get(header::ORIGIN)
        && !origin.to_str().is_ok_and(is_loopback_origin)
    {
        return Some("the Origin header is not a loopback origin");
    }
    None
}

/// `scheme://authority` with a loopback authority. `null` (an opaque
/// origin: a sandboxed frame, a `file:` page) is not.
fn is_loopback_origin(origin: &str) -> bool {
    origin
        .split_once("://")
        .is_some_and(|(_, authority)| is_loopback_authority(authority))
}

/// `host[:port]` where host is `localhost` or a loopback IP (`[::1]` for v6).
fn is_loopback_authority(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        match rest.split_once(']') {
            Some((v6, port)) if port.is_empty() || is_port(port) => v6,
            _ => return false,
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) if is_port(port) => host,
            Some(_) => return false,
            None => authority,
        }
    };
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// `:<digits>`, the port suffix of an authority.
fn is_port(suffix: &str) -> bool {
    suffix
        .strip_prefix(':')
        .unwrap_or(suffix)
        .parse::<u16>()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_authorities() {
        for ok in [
            "localhost",
            "LOCALHOST:8420",
            "127.0.0.1",
            "127.0.0.1:1",
            "127.1.2.3:80",
            "[::1]",
            "[::1]:8420",
        ] {
            assert!(is_loopback_authority(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "evil.example:8420",
            "localhost.evil.example",
            "10.0.0.1:80",
            "0.0.0.0:80",
            "[::2]:80",
            "[::1]x",
            "localhost:notaport",
            "",
        ] {
            assert!(!is_loopback_authority(bad), "{bad}");
        }
    }

    #[test]
    fn loopback_origins() {
        assert!(is_loopback_origin("http://localhost:3000"));
        assert!(is_loopback_origin("http://127.0.0.1"));
        assert!(!is_loopback_origin("null"));
        assert!(!is_loopback_origin("https://evil.example"));
    }
}
