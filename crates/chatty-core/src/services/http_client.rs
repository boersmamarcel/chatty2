//! Centralised HTTP client factory.
//!
//! All `reqwest::Client` instances should be created through these helpers so
//! that user-agent strings, timeouts, and redirect policies are consistent
//! across the codebase.

use std::sync::LazyLock;
use std::time::Duration;

/// Default user-agent for outgoing HTTP requests.
pub const USER_AGENT: &str = "Chatty/1.0 (Desktop AI Assistant)";

/// Browser-like user-agent for web scraping (e.g. DuckDuckGo fallback).
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

/// Build a standard HTTP client with the Chatty user-agent and the given
/// timeout.
pub fn default_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(USER_AGENT)
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
        .build()
        .expect("Failed to initialize HTTP client (TLS backend error)")
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

    #[test]
    fn llm_client_returns_same_instance() {
        let a = llm_client() as *const reqwest::Client;
        let b = llm_client() as *const reqwest::Client;
        assert_eq!(a, b);
    }
}
