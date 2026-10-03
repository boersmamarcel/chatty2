//! The registry's token key and how a client trusts it (CX-0b, AGE-823).
//!
//! hive-registry signs access tokens, step-up assertions and billing
//! session tokens with one Ed25519 (EdDSA) key derived from its root key;
//! there is no shared secret and no HS256 any more (CX-0, hive#197). A
//! client trusts that key only through the root: the registry serves the
//! key with the root's signature over it at
//! `GET /.well-known/hive-session-key`, and the client checks that
//! signature against the root keys it already holds ([`crate::trust`], the
//! SEC-3 list). Nothing the registry says about its own key is trusted on
//! its own.
//!
//! The certified message starts with a fixed domain line, never with `{`,
//! so it can never be read as a publisher certificate (canonical JSON,
//! [`crate::verify`]) and a publisher certificate can never be read as one.

use base64::{
    Engine,
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::verify::parse_public_key;

/// The domain line the root signs ahead of the session key's hex. Must
/// match hive-registry's `SESSION_KEY_CERT_DOMAIN`.
pub const SESSION_KEY_CERT_DOMAIN: &[u8] = b"hive-registry/session-key-cert/v1\n";

/// What the registry serves at `GET /.well-known/hive-session-key`, and
/// what a billing session token carries in its `skey`/`skey_sig` claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionKeyCertificate {
    /// The session public key, hex Ed25519.
    pub public_key: String,
    /// The root's base64 signature over [`certificate_message`].
    pub signature: String,
}

/// The bytes the root signs to certify `public_key_hex`.
pub fn certificate_message(public_key_hex: &str) -> Vec<u8> {
    let mut message = SESSION_KEY_CERT_DOMAIN.to_vec();
    message.extend_from_slice(public_key_hex.as_bytes());
    message
}

/// Why a token or a key was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("no registry root key is trusted for this registry")]
    NoTrustedRoot,
    #[error("the session key is not certified by a trusted root")]
    UncertifiedKey,
    #[error("malformed token: {0}")]
    Malformed(&'static str),
    #[error("the token is not EdDSA-signed (alg `{0}`)")]
    WrongAlgorithm(String),
    #[error("the token's signature does not verify under the registry's session key")]
    BadSignature,
    #[error("the token has expired")]
    Expired,
}

impl SessionKeyCertificate {
    /// The session key, if any one of `roots` (hex Ed25519) signed this
    /// certificate. An empty `roots` refuses everything.
    pub fn verify(&self, roots: &[String]) -> Result<VerifyingKey, TokenError> {
        if roots.is_empty() {
            return Err(TokenError::NoTrustedRoot);
        }
        let public_key = self.public_key.to_ascii_lowercase();
        let key = parse_public_key(&public_key).map_err(|_| TokenError::UncertifiedKey)?;
        let signature = BASE64
            .decode(self.signature.trim())
            .ok()
            .and_then(|bytes| Signature::from_slice(&bytes).ok())
            .ok_or(TokenError::UncertifiedKey)?;
        let message = certificate_message(&public_key);
        let certified = roots.iter().any(|root| {
            parse_public_key(root)
                .map(|root| root.verify(&message, &signature).is_ok())
                .unwrap_or(false)
        });
        if certified {
            Ok(key)
        } else {
            Err(TokenError::UncertifiedKey)
        }
    }
}

#[derive(Deserialize)]
struct Header {
    alg: String,
}

#[derive(Deserialize)]
struct Expiry {
    exp: i64,
}

/// The claims of `token` if it is an EdDSA JWT signed by `key` and not
/// expired at `now` (Unix seconds). Any other `alg` — HS256 above all, the
/// retired shared-secret scheme — is refused before the signature is
/// looked at.
pub fn verify_token<T: DeserializeOwned>(
    token: &str,
    key: &VerifyingKey,
    now: i64,
) -> Result<T, TokenError> {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed("expected three parts"));
    };
    let header: Header = URL_SAFE_NO_PAD
        .decode(header)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or(TokenError::Malformed("header"))?;
    if header.alg != "EdDSA" {
        return Err(TokenError::WrongAlgorithm(header.alg));
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|bytes| Signature::from_slice(&bytes).ok())
        .ok_or(TokenError::Malformed("signature"))?;
    let signed = &token[..token.rfind('.').unwrap_or(0)];
    key.verify(signed.as_bytes(), &signature)
        .map_err(|_| TokenError::BadSignature)?;
    let payload = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| TokenError::Malformed("payload"))?;
    let Expiry { exp } =
        serde_json::from_slice(&payload).map_err(|_| TokenError::Malformed("exp"))?;
    if exp <= now {
        return Err(TokenError::Expired);
    }
    serde_json::from_slice(&payload).map_err(|_| TokenError::Malformed("claims"))
}

/// The claims of a billing session token (hive-registry's
/// `BillingSessionClaims`), including the certificate it carries for the
/// key that signed it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BillingClaims {
    pub sid: String,
    pub uid: String,
    #[serde(rename = "mod")]
    pub module_name: String,
    pub ver: String,
    pub res: i64,
    pub exp: i64,
    pub skey: Option<String>,
    pub skey_sig: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::{Value, json};

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn hex_of(key: &SigningKey) -> String {
        hex::encode(key.verifying_key().to_bytes())
    }

    /// `session` certified by `root`, as the registry serves it.
    fn certificate(root: &SigningKey, session: &SigningKey) -> SessionKeyCertificate {
        let public_key = hex_of(session);
        SessionKeyCertificate {
            signature: BASE64.encode(root.sign(&certificate_message(&public_key)).to_bytes()),
            public_key,
        }
    }

    /// A JWT over `claims` with header `alg`, signed by `key`.
    fn jwt(alg: &str, claims: &Value, key: &SigningKey) -> String {
        let header = URL_SAFE_NO_PAD.encode(json!({ "alg": alg, "typ": "JWT" }).to_string());
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let input = format!("{header}.{payload}");
        let signature = URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()).to_bytes());
        format!("{input}.{signature}")
    }

    #[test]
    fn a_session_key_certified_by_a_listed_root_verifies() {
        let (root, other_root, session) = (key(1), key(2), key(3));
        let cert = certificate(&root, &session);
        // Any listed root may have signed it (rotation window).
        let key = cert
            .verify(&[hex_of(&other_root), hex_of(&root)])
            .expect("certified");
        assert_eq!(key, session.verifying_key());
        assert_eq!(
            cert.verify(&[hex_of(&other_root)]),
            Err(TokenError::UncertifiedKey)
        );
        assert_eq!(cert.verify(&[]), Err(TokenError::NoTrustedRoot));
    }

    #[test]
    fn a_certificate_for_another_key_is_refused() {
        let (root, session, swapped) = (key(1), key(3), key(4));
        let mut cert = certificate(&root, &session);
        cert.public_key = hex_of(&swapped);
        assert_eq!(
            cert.verify(&[hex_of(&root)]),
            Err(TokenError::UncertifiedKey)
        );
    }

    #[test]
    fn an_eddsa_token_verifies_and_hs256_or_a_foreign_key_does_not() {
        let (session, foreign) = (key(3), key(5));
        let claims = json!({ "sub": "u", "exp": 2_000 });
        let token = jwt("EdDSA", &claims, &session);
        let got: Value = verify_token(&token, &session.verifying_key(), 1_000).unwrap();
        assert_eq!(got["sub"], "u");

        let foreign_token = jwt("EdDSA", &claims, &foreign);
        assert_eq!(
            verify_token::<Value>(&foreign_token, &session.verifying_key(), 1_000),
            Err(TokenError::BadSignature)
        );
        let hs256 = jwt("HS256", &claims, &session);
        assert_eq!(
            verify_token::<Value>(&hs256, &session.verifying_key(), 1_000),
            Err(TokenError::WrongAlgorithm("HS256".into()))
        );
        assert_eq!(
            verify_token::<Value>(&token, &session.verifying_key(), 2_000),
            Err(TokenError::Expired)
        );
        assert!(matches!(
            verify_token::<Value>("a.b", &session.verifying_key(), 0),
            Err(TokenError::Malformed(_))
        ));
    }
}
