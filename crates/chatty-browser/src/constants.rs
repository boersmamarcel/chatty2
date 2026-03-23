//! Shared constants used across the chatty-browser crate.

// ── Content limits ──────────────────────────────────────────────────────────

/// Maximum characters for `text_content` in a page snapshot.
pub const MAX_TEXT_CONTENT_LEN: usize = 3_000;
/// Maximum number of interactive elements to include.
pub const MAX_ELEMENTS: usize = 50;
/// Maximum number of links to include.
pub const MAX_LINKS: usize = 50;

// ── User agent ──────────────────────────────────────────────────────────────

/// Realistic Chrome user-agent string shared between the WebView backend
/// and the HTTP fallback.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

// ── Timeouts & timing ───────────────────────────────────────────────────────

/// Default page-load timeout used by the auth tool (milliseconds).
pub const PAGE_LOAD_TIMEOUT_MS: u64 = 15_000;
/// Delay after page load for JS framework hydration (milliseconds).
pub const HYDRATION_DELAY_MS: u64 = 1_000;
/// Pause between filling form fields (milliseconds).
pub const INTER_FIELD_DELAY_MS: u64 = 300;

// ── Wait-for-load polling ───────────────────────────────────────────────────

/// Interval between readyState / content-length polls (milliseconds).
pub const POLL_INTERVAL_MS: u64 = 500;
/// Body must have at least this many chars before we consider it "loaded".
pub const MIN_CONTENT_LENGTH: usize = 100;
/// Number of consecutive same-length polls required to declare stability.
pub const STABLE_CHECK_COUNT: u32 = 3;
/// Maximum time (seconds) spent in the content-stabilization phase.
pub const MAX_STABILIZE_SECS: u64 = 10;

// ── IPC protocol (wry backend) ──────────────────────────────────────────────

/// Prefix prepended to JS evaluation results sent through the IPC channel.
pub const IPC_RESULT_PREFIX: &str = "__chatty_js_result:";
/// Prefix that marks a JS evaluation error inside the IPC payload.
pub const JS_ERROR_PREFIX: &str = "__error:";
/// Initial URL loaded in every new tab.
pub const INITIAL_TAB_URL: &str = "about:blank";

// ── HTTP headers ────────────────────────────────────────────────────────────

/// Build the default set of browser-like HTTP headers.
///
/// Used by both the HTTP fallback and the auth tool's HTTP path to avoid
/// bot detection. All values are compile-time constants, so `expect` is safe.
pub fn default_browser_headers() -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        reqwest::header::ACCEPT,
        "text/html,application/xhtml+xml,application/xml;q=0.9,\
         image/avif,image/webp,image/apng,*/*;q=0.8"
            .parse()
            .expect("valid Accept header"),
    );
    h.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        "en-US,en;q=0.9"
            .parse()
            .expect("valid Accept-Language header"),
    );
    h.insert(
        reqwest::header::ACCEPT_ENCODING,
        "gzip, deflate, br"
            .parse()
            .expect("valid Accept-Encoding header"),
    );
    h.insert(
        "Sec-Fetch-Dest",
        "document".parse().expect("valid Sec-Fetch-Dest header"),
    );
    h.insert(
        "Sec-Fetch-Mode",
        "navigate".parse().expect("valid Sec-Fetch-Mode header"),
    );
    h.insert(
        "Sec-Fetch-Site",
        "none".parse().expect("valid Sec-Fetch-Site header"),
    );
    h.insert(
        "Sec-Fetch-User",
        "?1".parse().expect("valid Sec-Fetch-User header"),
    );
    h.insert(
        "Upgrade-Insecure-Requests",
        "1".parse().expect("valid Upgrade-Insecure-Requests header"),
    );
    h
}
