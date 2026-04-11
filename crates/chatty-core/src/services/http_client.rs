//! Centralised HTTP client factory.
//!
//! All `reqwest::Client` instances should be created through these helpers so
//! that user-agent strings, timeouts, and redirect policies are consistent
//! across the codebase.

use std::time::Duration;

/// Default user-agent for outgoing HTTP requests.
pub const USER_AGENT: &str = "Chatty/1.0 (Desktop AI Assistant)";

/// Build a standard HTTP client with the Chatty user-agent and the given
/// timeout.
pub fn default_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent(USER_AGENT)
        .build()
        .expect("Failed to build HTTP client")
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
        .expect("Failed to build HTTP client")
}

/// Build a minimal HTTP client (no custom user-agent) for probing endpoints.
///
/// Used for short-lived metadata requests where a branded user-agent is not
/// needed (e.g. OAuth well-known discovery).
pub fn probe_client(timeout_secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .expect("Failed to build HTTP client")
}
