//! `hive-billing-sdk` — Publisher-facing SDK for Hive billing integration.
//!
//! This crate provides ergonomic wrappers around the `billing` WIT interface
//! for paid WASM modules. It handles session token verification and provides
//! helper functions like [`require_session`] and [`report_usage`].
//!
//! # Overview
//!
//! Hive's billing model uses signed session tokens so that a module can check
//! that the host really reserved credits before it does any work, even though
//! the user controls the runtime the module runs in.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use chatty_module_sdk::{ToolCallRequest, ToolError, ToolResult};
//! use hive_billing_sdk::{require_session, report_usage};
//!
//! // Inside a plugin's `invoke_tool`:
//! fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
//!     // Verify session and reserve credits (5000 tokens estimated)
//!     let session = require_session(5000).map_err(ToolError::denied)?;
//!
//!     // ... do actual work: call the LLM, process the request ...
//!
//!     // Report actual usage
//!     report_usage(&session, actual_input, actual_output).map_err(ToolError::failed)?;
//!     Ok(result)
//! }
//! ```
//!
//! # Trust model (CX-0b, AGE-823)
//!
//! The session token is an **EdDSA** (Ed25519) JWT signed by hive-registry's
//! session key. There is no shared secret: HS256 is refused outright.
//!
//! The module never takes the session key on the host's word. Every token
//! carries the key that signed it (`skey`, hex) and the registry root's
//! signature certifying that key (`skey_sig`, base64, over
//! `hive-registry/session-key-cert/v1\n` followed by the key's hex). The
//! module verifies, in order:
//!
//! 1. `skey_sig` under one of the **registry root public keys** it trusts
//!    ([`PRODUCTION_ROOT_PUBLIC_KEYS`], the same SEC-3 list chatty compiles
//!    in, or the keys given to [`configure_root_keys`]);
//! 2. the token's EdDSA signature under `skey`;
//! 3. that the token has not expired, and that it reserves what was asked.
//!
//! Only public keys are embedded in the module, so nothing in the binary is
//! worth extracting.
//!
//! # Configuration
//!
//! A module built for the production registry needs nothing: it trusts the
//! compiled [`PRODUCTION_ROOT_PUBLIC_KEYS`]. A module tested against a local
//! registry (the hive docker compose stack) names that registry's root key
//! once, at initialisation:
//!
//! ```rust,ignore
//! hive_billing_sdk::configure_root_keys(&[env!("HIVE_DEV_ROOT_PUBLIC_KEY")]);
//! ```

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The production registry root public keys (hex Ed25519), compiled in.
/// Mirrors `hive_client::trust::PRODUCTION_ROOT_PUBLIC_KEYS` (SEC-3,
/// AGE-816); a test in `hive-client` keeps the two equal. Empty until the
/// real key is pinned, so until then every token is refused unless
/// [`configure_root_keys`] names a root.
pub const PRODUCTION_ROOT_PUBLIC_KEYS: &[&str] = &[];

/// The domain line the root signs ahead of the session key's hex. Must match
/// hive-registry's `SESSION_KEY_CERT_DOMAIN`.
pub const SESSION_KEY_CERT_DOMAIN: &[u8] = b"hive-registry/session-key-cert/v1\n";

static ROOT_KEYS: OnceLock<Vec<String>> = OnceLock::new();

/// Trust `keys` (hex Ed25519 registry root public keys) instead of
/// [`PRODUCTION_ROOT_PUBLIC_KEYS`]. Call once, at module initialisation,
/// before [`require_session`]; for a local registry only.
///
/// # Panics
///
/// If called twice.
pub fn configure_root_keys(keys: &[&str]) {
    ROOT_KEYS
        .set(keys.iter().map(|k| k.trim().to_ascii_lowercase()).collect())
        .unwrap_or_else(|_| panic!("root keys already configured"));
}

/// The root keys tokens are verified against.
fn root_keys() -> Vec<String> {
    match ROOT_KEYS.get() {
        Some(keys) => keys.clone(),
        None => PRODUCTION_ROOT_PUBLIC_KEYS
            .iter()
            .map(|k| k.to_ascii_lowercase())
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Session Token Verification
// ---------------------------------------------------------------------------

/// Claims embedded in a Hive billing session JWT.
///
/// Matches `BillingSessionClaims` in `hive-registry/src/models.rs`.
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
    /// The session public key that signed the token (hex).
    pub skey: String,
    /// The root's base64 signature certifying `skey`.
    pub skey_sig: String,
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

fn parse_key(hex_key: &str) -> Option<ed25519_dalek::VerifyingKey> {
    let bytes: [u8; 32] = decode_hex(hex_key)?.try_into().ok()?;
    ed25519_dalek::VerifyingKey::from_bytes(&bytes).ok()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Verify a billing session token against `roots` (hex Ed25519 registry
/// root public keys) at `now` (Unix seconds): the root certifies the key in
/// `skey`, the key signed the token (EdDSA, never HS256), and it has not
/// expired. Returns the verified claims.
///
/// [`require_session`] calls this with the trusted roots; it is public so a
/// module can check a token it got some other way.
pub fn verify_session_token(
    token: &str,
    roots: &[String],
    now: i64,
) -> Result<SessionClaims, String> {
    use base64::{
        Engine,
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    };
    use ed25519_dalek::{Signature, Verifier};

    if roots.is_empty() {
        return Err(
            "no registry root key is trusted: call configure_root_keys() for a local registry"
                .to_string(),
        );
    }
    let parts: Vec<&str> = token.split('.').collect();
    let [header, payload, signature] = parts[..] else {
        return Err("Invalid JWT format: expected 3 parts".to_string());
    };

    #[derive(Deserialize)]
    struct Header {
        alg: String,
    }
    let header: Header = URL_SAFE_NO_PAD
        .decode(header)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| "Invalid JWT header".to_string())?;
    if header.alg != "EdDSA" {
        return Err(format!(
            "Session token is not EdDSA-signed (alg `{}`)",
            header.alg
        ));
    }

    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|e| format!("Failed to decode JWT payload: {}", e))?;
    let claims: SessionClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|e| format!("Failed to parse JWT claims: {}", e))?;

    // 1. The root certifies the key the token names.
    let skey_hex = claims.skey.to_ascii_lowercase();
    let skey = parse_key(&skey_hex).ok_or_else(|| "Invalid session key".to_string())?;
    let cert_sig = STANDARD
        .decode(claims.skey_sig.trim())
        .ok()
        .and_then(|b| Signature::from_slice(&b).ok())
        .ok_or_else(|| "Invalid session key certificate".to_string())?;
    let mut message = SESSION_KEY_CERT_DOMAIN.to_vec();
    message.extend_from_slice(skey_hex.as_bytes());
    let certified = roots.iter().any(|root| {
        parse_key(root)
            .map(|root| root.verify(&message, &cert_sig).is_ok())
            .unwrap_or(false)
    });
    if !certified {
        return Err("Session key is not certified by a trusted registry root".to_string());
    }

    // 2. The key signed the token.
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|b| Signature::from_slice(&b).ok())
        .ok_or_else(|| "Invalid signature encoding".to_string())?;
    let signed = format!("{}.{}", parts[0], parts[1]);
    skey.verify(signed.as_bytes(), &signature)
        .map_err(|_| "JWT signature verification failed".to_string())?;

    // 3. Unexpired.
    if claims.exp <= now {
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
/// 2. Verifies the returned token ([`verify_session_token`]) against the
///    trusted registry root keys
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
/// - `Err(String)` — Verification failed, or insufficient credits, or no trusted registry root
///
/// # Example
///
/// ```rust,ignore
/// use hive_billing_sdk::require_session;
///
/// fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
///     // Reserve 5000 tokens
///     let session = require_session(5000).map_err(ToolError::denied)?;
///
///     // Token is verified — safe to proceed
///     // ...
/// }
/// ```
///
/// # Errors
///
/// This function returns an error if:
/// - No registry root key is trusted (see [`configure_root_keys`])
/// - The host returns an error (insufficient credits, network failure)
/// - The session key is not certified by a trusted root, or the token is
///   not EdDSA-signed by it
/// - The JWT has expired
/// - The JWT claims are malformed
pub fn require_session(estimated_tokens: i64) -> Result<BillingSession, String> {
    // Call the host import to acquire a session
    let session_info = billing_acquire_session(estimated_tokens)?;

    // Verify the token under the trusted registry roots
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "System time error".to_string())?
        .as_secs() as i64;
    let claims = verify_session_token(&session_info.token, &root_keys(), now)?;

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
/// fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
///     let session = require_session(5000).map_err(ToolError::denied)?;
///
///     // ... do work ...
///
///     report_usage(&session, 1200, 800).map_err(ToolError::failed)?;
///     Ok(result)
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
// Host imports (from chatty-module-sdk)
// ---------------------------------------------------------------------------

use chatty_module_sdk::billing::SessionInfo;

/// The `billing::acquire-session` host import.
fn billing_acquire_session(estimated_tokens: i64) -> Result<SessionInfo, String> {
    chatty_module_sdk::billing::acquire_session(estimated_tokens)
}

/// The `billing::report-usage` host import.
fn billing_report_usage(input_tokens: i64, output_tokens: i64) -> Result<(), String> {
    chatty_module_sdk::billing::report_usage(input_tokens, output_tokens)
}

// Re-export for users who want direct access
pub use base64;
pub use serde_json;
