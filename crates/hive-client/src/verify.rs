//! Verification of Hive module downloads (PL-H5, AGE-608).
//!
//! Hive signs modules with a two-link chain anchored in the **registry root
//! key**, which the consumer holds and never takes from a download:
//!
//! ```text
//! registry root key ──signs──▶ publisher certificate {not_before, public_key, publisher_id}
//! publisher key     ──signs──▶ signed manifest {capabilities, name, sha256, version, wit_version}
//! manifest.sha256   ──equals─▶ SHA-256 of the downloaded .wasm
//! ```
//!
//! Both signed documents are [canonical JSON](canonical_json). This module
//! mirrors hive's reference verifier (`services/hive-verify`) byte for byte;
//! `tests/vectors/` holds the vectors both repositories check against, so the
//! two cannot drift apart silently. Which root key a client trusts is
//! [`crate::trust`]'s business.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

// ── Download headers ───────────────────────────────────────────────────────

/// Base64 of the canonical signed manifest.
pub const HEADER_MANIFEST: &str = "X-Hive-Manifest";
/// Base64 Ed25519 signature of the publisher key over the manifest bytes.
pub const HEADER_MANIFEST_SIGNATURE: &str = "X-Hive-Manifest-Signature";
/// Base64 of the canonical publisher certificate.
pub const HEADER_CERTIFICATE: &str = "X-Hive-Publisher-Certificate";
/// Base64 Ed25519 signature of the registry root key over the certificate bytes.
pub const HEADER_CERTIFICATE_SIGNATURE: &str = "X-Hive-Certificate-Signature";

// ── Public types ───────────────────────────────────────────────────────────

/// Level of trust assigned to a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// Module the user put on disk by hand; no signature required.
    Local,
    /// Module from the registry whose chain verifies against the root key.
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

/// The capabilities a module requests, as the publisher signed them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub agent: bool,
    pub chat: bool,
    pub tools: Vec<String>,
}

/// What the publisher's signature covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest {
    pub capabilities: Capabilities,
    pub name: String,
    /// Lowercase hex SHA-256 of the `.wasm` binary.
    pub sha256: String,
    pub version: String,
    /// The WIT package version the component exports, e.g. `0.3.0`.
    pub wit_version: String,
}

/// What the registry root key signs when it certifies a publisher key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherCertificate {
    /// Unix seconds (UTC) at which the root certified the key.
    pub not_before: i64,
    /// Lowercase hex Ed25519 verifying key of the publisher.
    pub public_key: String,
    /// The publisher's registry user id (hyphenated lowercase UUID).
    pub publisher_id: String,
}

/// Everything a download carries besides the `.wasm`: the two signed
/// documents, as their exact canonical text, and their signatures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleChain {
    /// Canonical JSON of the [`PublisherCertificate`].
    pub certificate: String,
    /// Base64 root-key signature over `certificate`.
    pub certificate_signature: String,
    /// Canonical JSON of the [`SignedManifest`].
    pub manifest: String,
    /// Base64 publisher-key signature over `manifest`.
    pub manifest_signature: String,
}

/// A chain that verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedModule {
    pub certificate: PublisherCertificate,
    pub manifest: SignedManifest,
}

/// Why a download did not verify.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("missing {0}")]
    Missing(&'static str),
    #[error("invalid public key: {0}")]
    InvalidPublicKey(String),
    #[error("invalid signature encoding: {0}")]
    InvalidSignature(String),
    #[error("invalid {0} encoding: {1}")]
    InvalidEncoding(&'static str, String),
    #[error("the publisher certificate is not signed by the registry root key")]
    CertificateSignatureMismatch,
    #[error("the manifest is not signed by the certified publisher key")]
    ManifestSignatureMismatch,
    #[error("malformed {0}: {1}")]
    Malformed(&'static str, String),
    #[error("the {0} is not in canonical form")]
    NotCanonical(&'static str),
    #[error("hash mismatch: the wasm's SHA-256 does not match the signed manifest")]
    HashMismatch,
    #[error("the signed manifest is for {got}, not the requested {expected}")]
    WrongModule { expected: String, got: String },
}

// ── Canonical JSON ─────────────────────────────────────────────────────────

/// The canonical JSON bytes of `value`: object keys sorted by their UTF-8
/// bytes at every level, no whitespace, strings escaped as `serde_json`
/// does. Array order is kept. Sorting here, rather than relying on
/// `serde_json::Map`, keeps the bytes the same with `preserve_order` on.
pub fn canonical_json<T: Serialize>(value: &T) -> Vec<u8> {
    let value = serde_json::to_value(value).expect("the signed documents serialize to JSON");
    let mut out = Vec::new();
    write_canonical(&value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            out.push(b'{');
            for (i, (key, value)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                out.extend(serde_json::to_vec(key).expect("a string serializes"));
                out.push(b':');
                write_canonical(value, out);
            }
            out.push(b'}');
        }
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_canonical(item, out);
            }
            out.push(b']');
        }
        scalar => out.extend(serde_json::to_vec(scalar).expect("a scalar serializes")),
    }
}

impl SignedManifest {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_json(self)
    }
}

impl PublisherCertificate {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        canonical_json(self)
    }
}

/// Parse a signed document and refuse it unless `text` is its canonical form.
fn parse_canonical<T: Serialize + DeserializeOwned>(
    what: &'static str,
    text: &str,
) -> Result<T, VerifyError> {
    let parsed: T =
        serde_json::from_str(text).map_err(|e| VerifyError::Malformed(what, e.to_string()))?;
    if canonical_json(&parsed) != text.as_bytes() {
        return Err(VerifyError::NotCanonical(what));
    }
    Ok(parsed)
}

// ── Chain verification ─────────────────────────────────────────────────────

impl ModuleChain {
    /// The chain from the four download header values ([`HEADER_MANIFEST`]
    /// and friends); `None` for a header the response did not carry.
    pub fn from_headers(
        manifest: Option<&str>,
        manifest_signature: Option<&str>,
        certificate: Option<&str>,
        certificate_signature: Option<&str>,
    ) -> Result<Self, VerifyError> {
        let document = |what: &'static str, value: Option<&str>| {
            let value = value.ok_or(VerifyError::Missing(what))?;
            let bytes = BASE64
                .decode(value)
                .map_err(|e| VerifyError::InvalidEncoding(what, e.to_string()))?;
            String::from_utf8(bytes).map_err(|e| VerifyError::InvalidEncoding(what, e.to_string()))
        };
        Ok(Self {
            manifest: document(HEADER_MANIFEST, manifest)?,
            manifest_signature: manifest_signature
                .ok_or(VerifyError::Missing(HEADER_MANIFEST_SIGNATURE))?
                .to_string(),
            certificate: document(HEADER_CERTIFICATE, certificate)?,
            certificate_signature: certificate_signature
                .ok_or(VerifyError::Missing(HEADER_CERTIFICATE_SIGNATURE))?
                .to_string(),
        })
    }
}

/// Parse a hex Ed25519 verifying key.
pub fn parse_public_key(hex_key: &str) -> Result<VerifyingKey, VerifyError> {
    let bytes = hex::decode(hex_key).map_err(|e| VerifyError::InvalidPublicKey(e.to_string()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| VerifyError::InvalidPublicKey("key must be 32 bytes".to_string()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|e| VerifyError::InvalidPublicKey(e.to_string()))
}

fn signature_holds(
    key: &VerifyingKey,
    data: &[u8],
    signature_b64: &str,
) -> Result<bool, VerifyError> {
    let bytes = BASE64
        .decode(signature_b64)
        .map_err(|e| VerifyError::InvalidSignature(e.to_string()))?;
    let bytes: [u8; 64] = bytes
        .try_into()
        .map_err(|_| VerifyError::InvalidSignature("signature must be 64 bytes".to_string()))?;
    Ok(key.verify(data, &Signature::from_bytes(&bytes)).is_ok())
}

/// Verify root → publisher certificate → manifest signature → wasm SHA-256,
/// in hive-verify's order. `root_public_key_hex` is the registry root key the
/// consumer trusts; it never comes from the download. [`verify_download`]
/// also checks the manifest is for the module that was asked for.
pub fn verify_chain(
    root_public_key_hex: &str,
    chain: &ModuleChain,
    wasm: &[u8],
) -> Result<VerifiedModule, VerifyError> {
    let root = parse_public_key(root_public_key_hex)?;
    if !signature_holds(
        &root,
        chain.certificate.as_bytes(),
        &chain.certificate_signature,
    )? {
        return Err(VerifyError::CertificateSignatureMismatch);
    }
    let certificate: PublisherCertificate =
        parse_canonical("publisher certificate", &chain.certificate)?;

    let publisher = parse_public_key(&certificate.public_key)?;
    if !signature_holds(
        &publisher,
        chain.manifest.as_bytes(),
        &chain.manifest_signature,
    )? {
        return Err(VerifyError::ManifestSignatureMismatch);
    }
    let manifest: SignedManifest = parse_canonical("signed manifest", &chain.manifest)?;

    if hex::encode(Sha256::digest(wasm)) != manifest.sha256 {
        return Err(VerifyError::HashMismatch);
    }
    Ok(VerifiedModule {
        certificate,
        manifest,
    })
}

/// [`verify_chain`], then the consumer's own step: the signed manifest must
/// name `name@version`, so another module's genuine download replayed under
/// this name is refused.
pub fn verify_download(
    root_public_key_hex: &str,
    chain: &ModuleChain,
    wasm: &[u8],
    name: &str,
    version: &str,
) -> Result<VerifiedModule, VerifyError> {
    let verified = verify_chain(root_public_key_hex, chain, wasm)?;
    if verified.manifest.name != name || verified.manifest.version != version {
        return Err(VerifyError::WrongModule {
            expected: format!("{name}@{version}"),
            got: format!("{}@{}", verified.manifest.name, verified.manifest.version),
        });
    }
    Ok(verified)
}

/// [`verify_download`] against each of `roots` in turn; succeeds as soon as
/// any one of them signed the certificate (SEC-3, AGE-816). This is what
/// makes a key-rotation window possible: chatty trusts the `current` and
/// `next` root at once, so a certificate signed by either verifies.
///
/// With an empty `roots` list there is nothing to trust, so this always
/// returns [`VerifyError::CertificateSignatureMismatch`].
pub fn verify_download_any(
    roots: &[String],
    chain: &ModuleChain,
    wasm: &[u8],
    name: &str,
    version: &str,
) -> Result<VerifiedModule, VerifyError> {
    let mut last_err = VerifyError::CertificateSignatureMismatch;
    for root in roots {
        match verify_download(root, chain, wasm, name, version) {
            Ok(verified) => return Ok(verified),
            Err(err) => last_err = err,
        }
    }
    Err(last_err)
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    const WASM: &[u8] = b"\0asm\x0d\0\x01\0";

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn sign(key: &SigningKey, data: &[u8]) -> String {
        BASE64.encode(key.sign(data).to_bytes())
    }

    fn certificate_for(key: &SigningKey) -> String {
        String::from_utf8(
            PublisherCertificate {
                not_before: 1_790_000_000,
                public_key: hex::encode(key.verifying_key().to_bytes()),
                publisher_id: "00000000-0000-4000-8000-000000000001".to_string(),
            }
            .canonical_bytes(),
        )
        .unwrap()
    }

    fn manifest(name: &str) -> String {
        String::from_utf8(
            SignedManifest {
                capabilities: Capabilities::default(),
                name: name.to_string(),
                sha256: hex::encode(Sha256::digest(WASM)),
                version: "1.0.0".to_string(),
                wit_version: "0.3.0".to_string(),
            }
            .canonical_bytes(),
        )
        .unwrap()
    }

    /// A good chain for `echo@1.0.0`, and the root and publisher keys that made it.
    fn chain() -> (SigningKey, SigningKey, ModuleChain) {
        let (root, publisher) = (key(1), key(2));
        let certificate = certificate_for(&publisher);
        let manifest = manifest("echo");
        let chain = ModuleChain {
            certificate_signature: sign(&root, certificate.as_bytes()),
            certificate,
            manifest_signature: sign(&publisher, manifest.as_bytes()),
            manifest,
        };
        (root, publisher, chain)
    }

    fn root_hex(root: &SigningKey) -> String {
        hex::encode(root.verifying_key().to_bytes())
    }

    #[test]
    fn a_good_chain_verifies() {
        let (root, publisher, chain) = chain();
        let verified = verify_download(&root_hex(&root), &chain, WASM, "echo", "1.0.0").unwrap();
        assert_eq!(
            verified.certificate.public_key,
            hex::encode(publisher.verifying_key().to_bytes())
        );
        assert_eq!(verified.manifest.name, "echo");
    }

    #[test]
    fn verify_chain_rejects_swapped_publisher_key() {
        let (root, _, good) = chain();
        let attacker = key(3);

        // Re-signed manifest under the real certificate.
        let resigned = ModuleChain {
            manifest_signature: sign(&attacker, good.manifest.as_bytes()),
            ..good.clone()
        };
        assert_eq!(
            verify_chain(&root_hex(&root), &resigned, WASM),
            Err(VerifyError::ManifestSignatureMismatch)
        );

        // The attacker's key in the certificate: the root no longer covers it.
        let swapped = ModuleChain {
            certificate: certificate_for(&attacker),
            manifest_signature: sign(&attacker, good.manifest.as_bytes()),
            ..good.clone()
        };
        assert_eq!(
            verify_chain(&root_hex(&root), &swapped, WASM),
            Err(VerifyError::CertificateSignatureMismatch)
        );

        // Certified by the attacker's own root: no better.
        let self_certified = ModuleChain {
            certificate_signature: sign(&key(4), swapped.certificate.as_bytes()),
            ..swapped
        };
        assert_eq!(
            verify_chain(&root_hex(&root), &self_certified, WASM),
            Err(VerifyError::CertificateSignatureMismatch)
        );
    }

    #[test]
    fn a_tampered_wasm_is_refused() {
        let (root, _, chain) = chain();
        let err = verify_chain(&root_hex(&root), &chain, b"\0asm\x0d\0\x01\x01").unwrap_err();
        assert_eq!(err, VerifyError::HashMismatch);
        assert!(err.to_string().contains("hash mismatch"));
    }

    #[test]
    fn a_signed_but_non_canonical_document_is_refused() {
        let (root, publisher, good) = chain();
        let spaced = good.manifest.replace(',', ", ");
        let chain = ModuleChain {
            manifest_signature: sign(&publisher, spaced.as_bytes()),
            manifest: spaced,
            ..good
        };
        assert_eq!(
            verify_chain(&root_hex(&root), &chain, WASM),
            Err(VerifyError::NotCanonical("signed manifest"))
        );
    }

    #[test]
    fn a_genuine_download_of_another_module_is_refused() {
        let (root, _, chain) = chain();
        assert_eq!(
            verify_download(&root_hex(&root), &chain, WASM, "spin", "1.0.0"),
            Err(VerifyError::WrongModule {
                expected: "spin@1.0.0".to_string(),
                got: "echo@1.0.0".to_string(),
            })
        );
        assert!(matches!(
            verify_download(&root_hex(&root), &chain, WASM, "echo", "1.0.1"),
            Err(VerifyError::WrongModule { .. })
        ));
    }

    #[test]
    fn missing_headers_are_named() {
        assert_eq!(
            ModuleChain::from_headers(None, Some("s"), Some("Yw=="), Some("s")),
            Err(VerifyError::Missing(HEADER_MANIFEST))
        );
        assert_eq!(
            ModuleChain::from_headers(Some("Yw=="), Some("s"), Some("Yw=="), None),
            Err(VerifyError::Missing(HEADER_CERTIFICATE_SIGNATURE))
        );
    }

    #[test]
    fn a_cert_signed_by_either_listed_root_verifies() {
        let (root, publisher, chain) = chain();
        let other_root = key(5);

        // The real root is second in the list: order must not matter.
        let roots = vec![root_hex(&other_root), root_hex(&root)];
        let verified = verify_download_any(&roots, &chain, WASM, "echo", "1.0.0").unwrap();
        assert_eq!(
            verified.certificate.public_key,
            hex::encode(publisher.verifying_key().to_bytes())
        );

        // And with the real root first.
        let roots = vec![root_hex(&root), root_hex(&other_root)];
        verify_download_any(&roots, &chain, WASM, "echo", "1.0.0").unwrap();
    }

    #[test]
    fn a_cert_signed_by_an_unlisted_root_is_refused() {
        let (_root, _publisher, chain) = chain();
        let roots = vec![root_hex(&key(5)), root_hex(&key(6))];
        assert_eq!(
            verify_download_any(&roots, &chain, WASM, "echo", "1.0.0"),
            Err(VerifyError::CertificateSignatureMismatch)
        );

        // No roots at all: refused the same way, not a panic.
        assert_eq!(
            verify_download_any(&[], &chain, WASM, "echo", "1.0.0"),
            Err(VerifyError::CertificateSignatureMismatch)
        );
    }

    #[test]
    fn canonical_json_sorts_nested_keys_and_drops_whitespace() {
        let value: Value =
            serde_json::from_str(r#"{ "b": 1, "a": { "z": [true, "é"], "y": null } }"#).unwrap();
        assert_eq!(
            canonical_json(&value),
            r#"{"a":{"y":null,"z":[true,"é"]},"b":1}"#.as_bytes()
        );
    }
}
