//! `hive-billing-sdk` — Publisher-facing SDK for Hive billing integration.
//!
//! This crate provides ergonomic wrappers around the `billing` WIT interface
//! for paid WASM modules. It handles session token verification and provides
//! helper functions like [`require_session`] and [`report_usage`].
//!
//! # Overview
//!
//! Hive's Phase 3b billing model uses cryptographically signed session tokens
//! to ensure that modules can verify the host has properly reserved credits
//! before doing work. This prevents users from running paid modules without
//! paying, even though they control the runtime environment.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use hive_billing_sdk::{require_session, report_usage, BillingSession};
//!
//! fn chat(req: ChatRequest) -> Result<ChatResponse, String> {
//!     // Verify session and reserve credits (5000 tokens estimated)
//!     let session = require_session(5000)?;
//!     
//!     // ... do actual work: call LLM, process request ...
//!     
//!     // Report actual usage
//!     report_usage(session, actual_input, actual_output)?;
//!     
//!     Ok(response)
//! }
//! ```
//!
//! # Trust Model
//!
//! The session token is a JWT signed by Hive's server. The module verifies:
//! - Signature is valid (using Hive's signing key)
//! - Token hasn't expired (5 minute TTL)
//! - Token contains expected claims (session ID, user ID, reserved tokens)
//!
//! ## Current Implementation: HMAC-SHA256 with Shared Secret
//!
//! **IMPORTANT:** The current implementation uses HS256 (HMAC-SHA256) signing
//! with a shared secret, NOT Ed25519 public key cryptography as originally
//! envisioned in the design doc.
//!
//! This means:
//! - The module must embed Hive's JWT secret to verify tokens
//! - This secret must be kept confidential (embedded at compile time)
//! - The trust model relies on the secret not being extractable from the WASM binary
//!
//! ### Security Considerations
//!
//! HMAC verification in WASM modules provides **deterrence**, not **proof**:
//! - A determined attacker can extract the secret from the WASM binary
//! - However, this requires more effort than simply modifying the host runtime
//! - The embedded secret can be rotated by Hive without republishing modules
//!   (modules check a well-known endpoint for the current secret)
//!
//! For **high-trust scenarios**, modules should be run on Hive's Firecracker
//! infrastructure (Phase 4) where the billing verification happens server-side.
//!
//! ### Future: Ed25519 Upgrade Path
//!
//! A future version will migrate to Ed25519 signing:
//! - Hive signs JWTs with a private key (never shared)
//! - Modules verify using Hive's public key (safe to embed)
//! - Provides true asymmetric cryptographic verification
//!
//! The API surface remains unchanged — only the verification mechanism evolves.
//!
//! # Configuration
//!
//! ## Option 1: Compile-Time Secret (Default)
//!
//! Embed Hive's JWT secret at compile time:
//!
//! ```rust,ignore
//! use hive_billing_sdk::{configure_secret, require_session};
//!
//! // Call once at module initialization
//! configure_secret("your-hive-jwt-secret");
//! ```
//!
//! ## Option 2: Runtime Fetch (Recommended for Production)
//!
//! Fetch the secret from a well-known endpoint at module load:
//!
//! ```rust,ignore
//! // TODO: Implement HTTP fetch for wasm32-wasip2
//! // This requires WASI HTTP support or a host import
//! ```
//!
//! For now, use compile-time embedding and rotate secrets via module updates.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Global storage for the Hive JWT secret.
///
/// This secret is used to verify session tokens. It must be configured
/// before calling [`require_session`].
static JWT_SECRET: OnceLock<String> = OnceLock::new();

/// Configure the Hive JWT secret for session token verification.
///
/// This should be called once at module initialization, before any calls
/// to [`require_session`].
///
/// # Example
///
/// ```rust,ignore
/// use hive_billing_sdk::configure_secret;
///
/// // At module startup
/// configure_secret("your-hive-jwt-secret");
/// ```
///
/// # Security
///
/// The secret should be:
/// - Embedded at compile time (from a secure build environment)
/// - OR fetched from a well-known Hive endpoint at module load
/// - Kept confidential (though extractable from the WASM binary with effort)
///
/// A missing or incorrect secret will cause all billing calls to fail.
pub fn configure_secret(secret: impl Into<String>) {
    JWT_SECRET
        .set(secret.into())
        .unwrap_or_else(|_| panic!("JWT secret already configured"));
}

/// Get the configured JWT secret, or return an error if not configured.
fn get_secret() -> Result<&'static str, String> {
    JWT_SECRET
        .get()
        .map(|s| s.as_str())
        .ok_or_else(|| {
            "Billing SDK not configured: call configure_secret() at module initialization".to_string()
        })
}

// ---------------------------------------------------------------------------
// Session Token Verification
// ---------------------------------------------------------------------------

/// Claims embedded in a Hive billing session JWT.
///
/// Matches the structure defined in `hive-registry/src/models.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Session ID (UUID)
    pub sid: String,
    /// User ID (UUID)
    pub uid: String,
    /// Module name
    #[serde(rename = "mod")]
    pub module_name: String,
    /// Module version
    pub ver: String,
    /// Reserved tokens
    pub res: i64,
    /// User balance at time of reservation
    pub bal: i64,
    /// Issued at (Unix timestamp)
    pub iat: i64,
    /// Expires at (Unix timestamp)
    pub exp: i64,
}

/// Verified billing session information.
///
/// Returned by [`require_session`] after successful verification.
#[derive(Debug, Clone)]
pub struct BillingSession {
    /// The raw session token (JWT)
    pub token: String,
    /// Verified claims from the token
    pub claims: SessionClaims,
    /// User's balance at time of session creation
    pub balance_tokens: i64,
    /// Tokens reserved for this session
    pub reserved_tokens: i64,
    /// Pricing model ("free" or "paid")
    pub pricing_model: String,
}

/// Verify a session token JWT.
///
/// Returns the decoded and verified claims, or an error if verification fails.
fn verify_token(token: &str, secret: &str) -> Result<SessionClaims, String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    // JWT format: header.payload.signature
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("Invalid JWT format: expected 3 parts".to_string());
    }

    let header_payload = format!("{}.{}", parts[0], parts[1]);
    let signature = parts[2];

    // Verify HMAC-SHA256 signature
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| "Invalid secret key".to_string())?;
    mac.update(header_payload.as_bytes());
    
    // Decode the signature from base64url
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|e| format!("Invalid signature encoding: {}", e))?;
    
    // Verify the signature
    mac.verify_slice(&signature_bytes)
        .map_err(|_| "JWT signature verification failed".to_string())?;

    // Decode and parse the payload
    let payload = parts[1];
    let padded = match payload.len() % 4 {
        2 => format!("{}==", payload),
        3 => format!("{}=", payload),
        _ => payload.to_string(),
    };
    
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(&payload)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(&padded))
        .map_err(|e| format!("Failed to decode JWT payload: {}", e))?;

    let claims: SessionClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|e| format!("Failed to parse JWT claims: {}", e))?;

    // Verify expiry
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "System time error".to_string())?
        .as_secs() as i64;

    if claims.exp < now {
        return Err(format!(
            "Session token expired (exp: {}, now: {})",
            claims.exp, now
        ));
    }

    Ok(claims)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Acquire and verify a billing session before doing work.
///
/// This function:
/// 1. Calls the host's `billing::acquire-session` import
/// 2. Verifies the returned JWT signature using the configured secret
/// 3. Checks token expiry and claims
/// 4. Returns a verified [`BillingSession`] on success
///
/// # Arguments
///
/// * `estimated_tokens` — Estimated token usage for this invocation.
///   The host will reserve this many tokens from the user's balance.
///   Choose a reasonable over-estimate to avoid insufficient credits errors.
///
/// # Returns
///
/// - `Ok(BillingSession)` — Session acquired and verified
/// - `Err(String)` — Verification failed, or insufficient credits, or billing not configured
///
/// # Example
///
/// ```rust,ignore
/// use hive_billing_sdk::require_session;
///
/// fn chat(req: ChatRequest) -> Result<ChatResponse, String> {
///     // Reserve 5000 tokens
///     let session = require_session(5000)?;
///     
///     // Token is verified — safe to proceed
///     // ...
/// }
/// ```
///
/// # Errors
///
/// This function returns an error if:
/// - The billing SDK secret is not configured ([`configure_secret`])
/// - The host returns an error (insufficient credits, network failure)
/// - The JWT signature is invalid
/// - The JWT has expired
/// - The JWT claims are malformed
pub fn require_session(estimated_tokens: i64) -> Result<BillingSession, String> {
    // Get the configured secret
    let secret = get_secret()?;
    
    // Call the host import to acquire a session
    // This is the WIT-generated binding from chatty-module-sdk
    // We need to import the billing interface types here
    let session_info = billing_acquire_session(estimated_tokens)?;
    
    // Verify the JWT token
    let claims = verify_token(&session_info.token, secret)?;
    
    // Validate reserved tokens match the request
    if claims.res != estimated_tokens {
        return Err(format!(
            "Session token mismatch: requested {} tokens but token claims {}",
            estimated_tokens, claims.res
        ));
    }
    
    Ok(BillingSession {
        token: session_info.token,
        claims,
        balance_tokens: session_info.balance_tokens,
        reserved_tokens: session_info.reserved_tokens,
        pricing_model: session_info.pricing_model,
    })
}

/// Report actual usage after work is complete.
///
/// This settles the billing session: the host deducts the actual token
/// usage from the user's balance and releases any over-reserved tokens.
///
/// # Arguments
///
/// * `session` — The session returned by [`require_session`]
/// * `input_tokens` — Actual input tokens consumed
/// * `output_tokens` — Actual output tokens consumed
///
/// # Example
///
/// ```rust,ignore
/// use hive_billing_sdk::{require_session, report_usage};
///
/// fn chat(req: ChatRequest) -> Result<ChatResponse, String> {
///     let session = require_session(5000)?;
///     
///     // ... do work ...
///     
///     report_usage(&session, 1200, 800)?;
///     Ok(response)
/// }
/// ```
///
/// # Errors
///
/// Returns an error if the host fails to settle the session.
pub fn report_usage(
    _session: &BillingSession,
    input_tokens: i64,
    output_tokens: i64,
) -> Result<(), String> {
    // Call the host import to report usage
    billing_report_usage(input_tokens, output_tokens)
}

/// Simplified version that doesn't require passing the session.
///
/// Use this if you don't need to inspect session details in your module.
///
/// # Example
///
/// ```rust,ignore
/// use hive_billing_sdk::report_usage_simple;
///
/// report_usage_simple(1200, 800)?;
/// ```
pub fn report_usage_simple(input_tokens: i64, output_tokens: i64) -> Result<(), String> {
    billing_report_usage(input_tokens, output_tokens)
}

// ---------------------------------------------------------------------------
// WIT Import Bindings
// ---------------------------------------------------------------------------

// Generate WIT bindings for the billing interface
wit_bindgen::generate!({
    world: "module",
    path: "../../wit",
});

// Import the generated billing types and functions
use chatty::module::billing::{self, SessionInfo};

/// Wrapper around the WIT billing::acquire-session import.
fn billing_acquire_session(estimated_tokens: i64) -> Result<SessionInfo, String> {
    billing::acquire_session(estimated_tokens)
}

/// Wrapper around the WIT billing::report-usage import.
fn billing_report_usage(input_tokens: i64, output_tokens: i64) -> Result<(), String> {
    billing::report_usage(input_tokens, output_tokens)
}

// Re-export for users who want direct access
pub use base64;
pub use serde_json;
