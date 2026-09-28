use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
// futures_util is needed for the Stream bound on BegunDownload::stream.
use futures_util;

// ── Module metadata ────────────────────────────────────────────────────────

/// Metadata about a module as returned by the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleMetadata {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub author: AuthorMetadata,
    pub latest_version: Option<String>,
    pub license: Option<String>,
    pub tags: Vec<String>,
    pub category: Option<String>,
    pub downloads: i64,
    pub pricing_model: String,
    pub execution_mode: String,
    pub homepage: Option<String>,
    pub support_email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Authorship information embedded in [`ModuleMetadata`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorMetadata {
    pub id: Uuid,
    pub username: String,
}

// ── Module list ────────────────────────────────────────────────────────────

/// A paginated list of modules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleList {
    pub items: Vec<ModuleMetadata>,
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
}

// ── Version info ───────────────────────────────────────────────────────────

/// Metadata about a specific published version.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub module_name: String,
    pub version: String,
    pub wasm_hash: String,
    pub wasm_size_bytes: i64,
    pub manifest: Value,
    pub published_at: DateTime<Utc>,
    pub signature: Option<String>,
    pub publisher_public_key: Option<String>,
}

/// A paginated list of versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionList {
    pub items: Vec<VersionInfo>,
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
}

// ── Download result ────────────────────────────────────────────────────────

/// The result of downloading a module from the registry.
#[derive(Debug, Clone)]
pub struct DownloadResult {
    /// Raw WebAssembly binary.
    pub wasm: Vec<u8>,
    /// Hex-encoded SHA-256 hash of `wasm`.
    pub wasm_hash: String,
    /// Trust level determined after signature verification.
    pub trust_level: crate::verify::TrustLevel,
    /// Base64-encoded Ed25519 signature (if signed).
    pub signature: Option<String>,
    /// Hex-encoded Ed25519 verifying key (if signed).
    pub publisher_public_key: Option<String>,
    /// The manifest JSON from the version record.
    pub manifest: Value,
}

/// The most bytes a module download may have (PL-D3): 64 MiB. A larger body
/// is refused while it streams, before it is all in memory.
pub const MAX_DOWNLOAD_BYTES: u64 = 64 << 20;

/// An in-progress download whose body has not yet been consumed.
///
/// Returned by [`HiveRegistryClient::begin_download`]; stream the body
/// chunk-by-chunk (reporting progress between each), then pass the
/// collected bytes + this value to [`HiveRegistryClient::finalize_download`].
pub struct BegunDownload {
    /// `Content-Length` from the response, or 0 if the server did not send one.
    pub total_size: u64,
    /// `x-wasm-sha256` response header (hex-encoded SHA-256), if present.
    pub registry_hash: Option<String>,
    /// `x-signature` response header (Ed25519 signature), if present.
    pub signature: Option<String>,
    /// `x-publisher-public-key` response header, if present.
    pub publisher_public_key: Option<String>,
    /// Streaming response body. Read it with [`BegunDownload::read_body`],
    /// which enforces [`MAX_DOWNLOAD_BYTES`].
    pub stream: std::pin::Pin<
        Box<dyn futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>,
    >,
}

impl BegunDownload {
    /// Read the whole body, at most [`MAX_DOWNLOAD_BYTES`] of it.
    ///
    /// Refuses a declared `Content-Length` over the cap before reading, and
    /// stops at the first chunk that would take the body past it, so an
    /// oversized download never sits in memory. `on_progress` gets the bytes
    /// read so far after each chunk.
    pub async fn read_body(
        &mut self,
        on_progress: impl FnMut(u64),
    ) -> Result<Vec<u8>, crate::ClientError> {
        read_capped(
            &mut self.stream,
            self.total_size,
            MAX_DOWNLOAD_BYTES,
            on_progress,
        )
        .await
    }
}

/// [`BegunDownload::read_body`] with the cap as a parameter.
pub(crate) async fn read_capped<S, E>(
    stream: &mut S,
    declared: u64,
    cap: u64,
    mut on_progress: impl FnMut(u64),
) -> Result<Vec<u8>, crate::ClientError>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    crate::ClientError: From<E>,
{
    use futures_util::StreamExt as _;

    if declared > cap {
        return Err(crate::ClientError::TooLarge { limit: cap });
    }
    let mut body = Vec::with_capacity(declared as usize);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len() as u64 + chunk.len() as u64 > cap {
            return Err(crate::ClientError::TooLarge { limit: cap });
        }
        body.extend_from_slice(&chunk);
        on_progress(body.len() as u64);
    }
    Ok(body)
}

// ── Authentication ─────────────────────────────────────────────────────────

/// The token pair `login`, `register` and `refresh` return: a one-hour access
/// JWT (`token`, sent as the Bearer) and the opaque 30-day `refresh_token`
/// that `POST /api/auth/refresh` exchanges for the next pair. Every refresh
/// rotates the refresh token; presenting a retired one revokes them all.
///
/// `Debug` is redacted: neither token may reach a log.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenPair {
    pub token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for TokenPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenPair")
            .field("token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl TokenPair {
    /// Extract the username from the JWT claims (base64-decoded payload).
    /// Returns `None` if the token is malformed or missing the `username` claim.
    pub fn username(&self) -> Option<String> {
        self.jwt_claim("username")
    }

    /// Extract the user ID (`sub` claim) from the JWT.
    pub fn user_id(&self) -> Option<String> {
        self.jwt_claim("sub")
    }

    fn jwt_claim(&self, key: &str) -> Option<String> {
        let payload = self.token.split('.').nth(1)?;
        // JWT uses base64url (no padding) — add padding and decode
        let padded = match payload.len() % 4 {
            2 => format!("{payload}=="),
            3 => format!("{payload}="),
            _ => payload.to_string(),
        };
        let bytes = base64::engine::general_purpose::URL_SAFE
            .decode(padded)
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(payload))
            .ok()?;
        let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        claims.get(key)?.as_str().map(|s| s.to_string())
    }
}

// ── Categories ─────────────────────────────────────────────────────────────

/// A module category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub module_count: i64,
}

/// Paginated list of categories.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryList {
    pub items: Vec<Category>,
}

// ── Usage tracking ─────────────────────────────────────────────────────────

/// A single usage event to report to the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageEvent {
    pub idempotency_key: String,
    pub module_name: String,
    pub module_version: String,
    #[serde(default = "default_event_type")]
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fuel_consumed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    pub occurred_at: DateTime<Utc>,
}

fn default_event_type() -> String {
    "invocation".to_string()
}

/// Batch of usage events sent to the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageReportRequest {
    pub events: Vec<UsageEvent>,
}

/// Response from the usage report endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageReportResponse {
    pub accepted: usize,
    pub duplicates: usize,
}

/// Summary of usage for a module over a time period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    pub module_name: String,
    pub period: String,
    pub invocation_count: i64,
    pub tool_call_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_fuel: i64,
    pub total_execution_ms: i64,
}

/// Paginated list of usage summaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummaryList {
    pub items: Vec<UsageSummary>,
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
}

// ── Credit balance ─────────────────────────────────────────────────────────

/// Credit balance for the authenticated user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreditBalance {
    pub balance_tokens: i64,
    pub lifetime_purchased_tokens: i64,
    pub lifetime_consumed_tokens: i64,
}

/// Pricing configuration for a module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModulePricingInfo {
    pub module_name: String,
    pub price_per_call: String,
    pub free_tier_calls: i32,
    pub updated_at: DateTime<Utc>,
}

// ── Billing session (Phase 3b) ─────────────────────────────────────────────

/// Request to acquire a billing session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcquireSessionRequest {
    pub module_name: String,
    pub module_version: String,
    pub estimated_tokens: i64,
}

/// Response from acquire-session — includes signed JWT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcquireSessionResponse {
    /// Unique session ID (UUID).
    pub session_id: String,
    /// Hive-signed JWT containing session claims.
    pub token: String,
    /// User's current balance after reservation.
    pub balance_tokens: i64,
    /// Tokens reserved for this session.
    pub reserved_tokens: i64,
    /// Module's pricing model.
    pub pricing_model: String,
}

/// Request to settle a billing session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettleSessionRequest {
    pub session_id: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// Response from settle-session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettleSessionResponse {
    pub session_id: String,
    pub tokens_deducted: i64,
    pub tokens_released: i64,
    pub balance_after: i64,
}

// ── Query parameters ───────────────────────────────────────────────────────

/// Optional filters for [`HiveRegistryClient::list_modules`].
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct ListParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(
        sizes: &[usize],
    ) -> impl futures_util::Stream<Item = Result<bytes::Bytes, crate::ClientError>> + Unpin {
        futures_util::stream::iter(
            sizes
                .iter()
                .map(|&n| Ok(bytes::Bytes::from(vec![0u8; n])))
                .collect::<Vec<_>>(),
        )
    }

    /// A body with no `Content-Length` is cut at the first chunk that would
    /// cross the cap; the chunks after it are never pulled.
    #[tokio::test]
    async fn read_capped_stops_at_the_cap_without_a_declared_length() {
        let mut stream = chunks(&[4, 4, 4, 4]);
        let mut seen = Vec::new();
        let result = read_capped(&mut stream, 0, 10, |n| seen.push(n)).await;
        assert!(
            matches!(result, Err(crate::ClientError::TooLarge { limit: 10 })),
            "{result:?}"
        );
        assert_eq!(seen, vec![4, 8], "the third chunk would cross the cap");
        let rest = futures_util::StreamExt::count(stream).await;
        assert_eq!(rest, 1, "the last chunk was never read");
    }

    #[tokio::test]
    async fn read_capped_refuses_a_declared_length_over_the_cap_before_reading() {
        let mut stream = chunks(&[1]);
        let result = read_capped(&mut stream, 11, 10, |_| {}).await;
        assert!(matches!(result, Err(crate::ClientError::TooLarge { .. })));
        assert_eq!(futures_util::StreamExt::count(stream).await, 1);
    }

    #[tokio::test]
    async fn read_capped_accepts_exactly_the_cap() {
        let mut stream = chunks(&[5, 5]);
        let body = read_capped(&mut stream, 10, 10, |_| {}).await.unwrap();
        assert_eq!(body.len(), 10);
    }
}
