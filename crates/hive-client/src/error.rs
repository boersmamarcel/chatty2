use thiserror::Error;

/// Errors returned by the Hive registry client.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The registry could not be reached (network unavailable, timeout, etc.).
    #[error("registry unreachable: {0}")]
    Offline(#[source] reqwest::Error),

    /// The server returned an unexpected HTTP status code.
    #[error("registry returned HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// The server requires authentication but no valid token was provided (401).
    #[error("authentication required (401)")]
    Unauthorized,

    /// The requested module or version was not found.
    #[error("module not found: {0}")]
    NotFound(String),

    /// The module's cryptographic signature could not be verified.
    #[error("signature verification failed: {0}")]
    SignatureInvalid(String),

    /// No registry root key is trusted for this registry, so nothing from it
    /// can be verified (PL-H5). There is no trust-on-first-use.
    #[error(
        "no trusted root key for registry {registry}: downloads from it are refused \
         (for a local registry, set CHATTY_HIVE_ROOT_KEY to its root public key)"
    )]
    NoTrustedRoot { registry: String },

    /// The download is larger than [`MAX_DOWNLOAD_BYTES`](crate::models::MAX_DOWNLOAD_BYTES)
    /// (declared up front or found while streaming); nothing past the cap
    /// was read.
    #[error("download too large: over the {limit}-byte cap")]
    TooLarge { limit: u64 },

    /// The response body could not be parsed.
    #[error("response parse error: {0}")]
    Parse(#[source] serde_json::Error),

    /// A local I/O error (e.g. writing cache to disk).
    #[error("I/O error: {0}")]
    Io(#[source] std::io::Error),

    /// Any other error from the HTTP layer.
    #[error("http error: {0}")]
    HttpTransport(#[source] reqwest::Error),

    /// A registry token or its session key did not verify (CX-0b).
    #[error("registry token refused: {0}")]
    Token(#[from] crate::session_key::TokenError),

    /// A step-up request expired before it was approved, or was not
    /// approved within the wait.
    #[error("the step-up request was not approved: {0}")]
    StepUpNotApproved(String),

    /// The registry's base URL is plain `http://` to a non-local address
    /// (SEC-16, AGE-756): refused before any request is sent, so a bearer
    /// token or password never has a chance to leave in clear text.
    #[error("insecure registry URL: {0}")]
    InsecureUrl(String),
}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_connect() || e.is_timeout() {
            ClientError::Offline(e)
        } else {
            ClientError::HttpTransport(e)
        }
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        ClientError::Parse(e)
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::Io(e)
    }
}

impl ClientError {
    /// Returns `true` when the error represents an offline/unreachable state
    /// and the caller can serve cached data instead.
    pub fn is_offline(&self) -> bool {
        matches!(self, ClientError::Offline(_))
    }
}
