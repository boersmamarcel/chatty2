//! Ed25519 signature verification for Hive modules.
//!
//! Hive uses Ed25519 key pairs per publisher. The registry signs the
//! hex-encoded SHA-256 hash of the `.wasm` binary with the publisher's
//! private key. The signature is Base64-encoded.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Level of trust assigned to a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// Module loaded from the local filesystem; no signature required.
    Local,
    /// Module from the registry with a valid Ed25519 signature.
    Signed,
    /// Signed + publisher identity verified (future).
    Verified,
}

impl std::fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustLevel::Local => write!(f, "local"),
            TrustLevel::Signed => write!(f, "signed"),
            TrustLevel::Verified => write!(f, "verified"),
        }
    }
}

/// Errors during module verification.
#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("invalid publisher public key: {0}")]
    InvalidPublicKey(String),
    #[error("invalid signature encoding: {0}")]
    InvalidSignature(String),
    #[error("signature verification failed")]
    SignatureMismatch,
}

/// Input for verifying a module's cryptographic signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyInput {
    /// Hex-encoded SHA-256 hash of the `.wasm` binary.
    pub wasm_hash: String,
    /// Base64-encoded Ed25519 signature over the `wasm_hash` bytes.
    pub signature: String,
    /// Hex-encoded Ed25519 verifying (public) key of the publisher.
    pub publisher_public_key: String,
}

/// Verify that the Ed25519 signature was created by the publisher's key
/// over the wasm hash. Returns `Ok(TrustLevel::Signed)` on success.
pub fn verify_module(input: &VerifyInput) -> Result<TrustLevel, VerifyError> {
    let key_bytes = hex::decode(&input.publisher_public_key)
        .map_err(|e| VerifyError::InvalidPublicKey(e.to_string()))?;
    let key_bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| VerifyError::InvalidPublicKey("key must be 32 bytes".to_string()))?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| VerifyError::InvalidPublicKey(e.to_string()))?;

    let sig_bytes = BASE64
        .decode(&input.signature)
        .map_err(|e| VerifyError::InvalidSignature(e.to_string()))?;
    let sig_bytes: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| VerifyError::InvalidSignature("signature must be 64 bytes".to_string()))?;
    let signature = Signature::from_bytes(&sig_bytes);

    verifying_key
        .verify(input.wasm_hash.as_bytes(), &signature)
        .map_err(|_| VerifyError::SignatureMismatch)?;

    Ok(TrustLevel::Signed)
}

/// End-to-end tamper check: recompute SHA-256, compare to expected hash,
/// then verify the Ed25519 signature.
pub fn verify_wasm_bytes(
    wasm_bytes: &[u8],
    expected_hash_hex: &str,
    signature: &str,
    publisher_public_key: &str,
) -> Result<TrustLevel, VerifyError> {
    use sha2::{Digest, Sha256};

    let computed = hex::encode(Sha256::digest(wasm_bytes));
    if computed != expected_hash_hex {
        return Err(VerifyError::SignatureMismatch);
    }

    verify_module(&VerifyInput {
        wasm_hash: computed,
        signature: signature.to_string(),
        publisher_public_key: publisher_public_key.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    fn make_keypair() -> (String, String) {
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();
        (
            hex::encode(signing_key.to_bytes()),
            hex::encode(verifying_key.to_bytes()),
        )
    }

    fn sign(signing_key_hex: &str, data: &[u8]) -> String {
        let key_bytes: [u8; 32] = hex::decode(signing_key_hex).unwrap().try_into().unwrap();
        let sk = SigningKey::from_bytes(&key_bytes);
        BASE64.encode(sk.sign(data).to_bytes())
    }

    #[test]
    fn valid_signature_returns_signed() {
        let (sk_hex, pk_hex) = make_keypair();
        let hash = "deadbeef01234567890abcdef01234567890abcdef01234567890abcdef01234567";
        let sig = sign(&sk_hex, hash.as_bytes());
        let result = verify_module(&VerifyInput {
            wasm_hash: hash.to_string(),
            signature: sig,
            publisher_public_key: pk_hex,
        });
        assert!(matches!(result, Ok(TrustLevel::Signed)));
    }

    #[test]
    fn tampered_hash_rejected() {
        let (sk_hex, pk_hex) = make_keypair();
        let hash = "aabbcc";
        let sig = sign(&sk_hex, hash.as_bytes());
        let result = verify_module(&VerifyInput {
            wasm_hash: "different_hash".to_string(),
            signature: sig,
            publisher_public_key: pk_hex,
        });
        assert!(result.is_err());
    }

    #[test]
    fn wrong_key_rejected() {
        let (sk_hex, _) = make_keypair();
        let (_, pk_hex2) = make_keypair();
        let hash = "somehash";
        let sig = sign(&sk_hex, hash.as_bytes());
        let result = verify_module(&VerifyInput {
            wasm_hash: hash.to_string(),
            signature: sig,
            publisher_public_key: pk_hex2,
        });
        assert!(result.is_err());
    }

    #[test]
    fn verify_wasm_bytes_full_flow() {
        use sha2::{Digest, Sha256};
        let wasm = b"\0asm\x01\0\0\0";
        let hash = hex::encode(Sha256::digest(wasm));
        let (sk_hex, pk_hex) = make_keypair();
        let sig = sign(&sk_hex, hash.as_bytes());
        let result = verify_wasm_bytes(wasm, &hash, &sig, &pk_hex);
        assert!(matches!(result, Ok(TrustLevel::Signed)));
    }
}
