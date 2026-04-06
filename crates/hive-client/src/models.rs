use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

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

// ── Authentication ─────────────────────────────────────────────────────────

/// Response from register/login endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokenResponse {
    pub token: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

impl AuthTokenResponse {
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
            .or_else(|_| {
                base64::engine::general_purpose::STANDARD.decode(payload)
            })
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
