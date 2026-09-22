//! Centralised HTTP client factory.
//!
//! All `reqwest::Client` instances should be created through these helpers so
//! that user-agent strings, timeouts, and redirect policies are consistent
//! across the codebase.

use std::sync::LazyLock;
use std::time::Duration;

/// Default user-agent for outgoing HTTP requests.
///
/// Includes a contact so sites with an automated-traffic policy (e.g. SEC
/// EDGAR, which rejects anything without one — AGE-496) have somewhere to
/// reach a human about this traffic, rather than blocking it outright.
/// Kept to the plain `Name contact@domain` shape EDGAR's own examples use —
/// reproduction on 2026-09-21 showed it 403s a UA that wraps the contact in
/// `(+https://...; ...)` instead.
pub const USER_AGENT: &str = concat!(
    "Chatty/",
    env!("CARGO_PKG_VERSION"),
    " research@example.com"
);

/// Browser-like user-agent for web scraping (e.g. DuckDuckGo fallback).
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Default `Accept-Language` for outgoing HTTP requests.
///
/// Without this header, upstream servers (search engines in particular) fall
/// back to guessing locale from the requester's IP geolocation, which on
/// this network returns Dutch search results and page chrome (AGE-508).
pub const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

/// Build a standard HTTP client with the Chatty user-agent and the given
/// timeout.
pub fn default_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(USER_AGENT)
        .default_headers(accept_language_header())
        .build()
        .expect("Failed to initialize HTTP client (TLS backend error)")
}

/// Build a client for a response whose *duration* cannot be known in advance.
///
/// A total timeout is the wrong instrument for a stream: it bounds the whole
/// exchange, so a delegation that parks while a human answers a question dies
/// on the transport rather than on the question's own deadline. What can
/// honestly be bounded is **silence** — `read` is the gap between bytes, and
/// an SSE server that sends keep-alives (as this app's broker does, every
/// 15 s) keeps a live-but-quiet stream well inside it.
pub fn streaming_client(connect: Duration, read: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .read_timeout(read)
        .user_agent(USER_AGENT)
        .build()
        .expect("Failed to initialize HTTP client (TLS backend error)")
}

/// Build an HTTP client that does **not** follow redirects.
///
/// Used by the fetch tool (to report redirects) and OAuth flows (to capture
/// redirect URIs).
pub fn no_redirect_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(USER_AGENT)
        .default_headers(accept_language_header())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("Failed to initialize HTTP client (TLS backend error)")
}

/// Build a minimal HTTP client (no custom user-agent) for probing endpoints.
///
/// Used for short-lived metadata requests where a branded user-agent is not
/// needed (e.g. OAuth well-known discovery).
pub fn probe_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .expect("Failed to initialize HTTP client (TLS backend error)")
}

/// Build an HTTP client with a browser-like user-agent for web scraping.
///
/// Used by the DuckDuckGo fallback search which requires a realistic
/// user-agent to avoid being blocked.
pub fn browser_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(BROWSER_USER_AGENT)
        .default_headers(accept_language_header())
        .build()
        .expect("Failed to initialize HTTP client (TLS backend error)")
}

/// A single-entry header map carrying [`ACCEPT_LANGUAGE`], for
/// `ClientBuilder::default_headers`.
fn accept_language_header() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        reqwest::header::HeaderValue::from_static(ACCEPT_LANGUAGE),
    );
    headers
}

static LLM_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .expect("Failed to initialize HTTP client (TLS backend error)")
});

/// Shared client for LLM provider traffic (OpenRouter, Ollama, Azure OpenAI).
///
/// One connection pool for the app's lifetime instead of a fresh one per
/// agent build, so conversations reuse TLS/TCP connections. Deliberately
/// carries **no total request timeout** — completion streams run for
/// minutes and a timeout here would kill them mid-stream.
pub fn llm_client() -> &'static reqwest::Client {
    &LLM_CLIENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn llm_client_returns_same_instance() {
        let a = llm_client() as *const reqwest::Client;
        let b = llm_client() as *const reqwest::Client;
        assert_eq!(a, b);
    }

    /// Spawn a one-shot local HTTP server, send a GET through `client`, and
    /// return the raw request text it received.
    ///
    /// This is the only way to prove a `ClientBuilder::default_headers`
    /// header actually reaches the wire: reqwest merges default headers into
    /// the request in `Client::execute_request` (i.e. at `send()` time), not
    /// in `RequestBuilder::build()`, so building a request without sending it
    /// (as the fetch tool's own header tests do for its per-request
    /// User-Agent override) would not see it.
    async fn capture_request(client: &reqwest::Client) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            text
        });
        client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("request to local test server should succeed");
        handle.join().unwrap()
    }

    /// AGE-508: `default_client`, `browser_client` and `no_redirect_client`
    /// all send `Accept-Language`, so upstream servers stop guessing locale
    /// from IP geolocation.
    #[tokio::test]
    async fn default_client_sends_accept_language() {
        let text = capture_request(&default_client(5)).await;
        assert!(
            text.to_lowercase().contains(&format!(
                "accept-language: {}",
                ACCEPT_LANGUAGE.to_lowercase()
            )),
            "got request: {text}"
        );
    }

    #[tokio::test]
    async fn browser_client_sends_accept_language() {
        let text = capture_request(&browser_client(5)).await;
        assert!(
            text.to_lowercase().contains(&format!(
                "accept-language: {}",
                ACCEPT_LANGUAGE.to_lowercase()
            )),
            "got request: {text}"
        );
    }

    #[tokio::test]
    async fn no_redirect_client_sends_accept_language() {
        let text = capture_request(&no_redirect_client(5)).await;
        assert!(
            text.to_lowercase().contains(&format!(
                "accept-language: {}",
                ACCEPT_LANGUAGE.to_lowercase()
            )),
            "got request: {text}"
        );
    }
}
