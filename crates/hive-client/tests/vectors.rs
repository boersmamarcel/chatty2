//! hive's byte-for-byte signing vectors (AGE-704), copied unchanged from
//! hive's `services/hive-verify/tests/vectors/`. hive-verify checks its own
//! verifier against the same files, so the two implementations cannot drift
//! apart silently (PL-H5, AGE-608).
//!
//! The keys in `chain.json` are TEST-ONLY (fixed, publicly committed seeds);
//! nothing may ever trust them.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signer, SigningKey};
use hive_client::verify::{
    Capabilities, ModuleChain, PublisherCertificate, SignedManifest, VerifyError, verify_chain,
    verify_download,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const VECTORS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vectors");

/// The TEST-ONLY seeds `chain.json` was made with.
const TEST_ONLY_ROOT_SEED: [u8; 32] = [0x0a; 32];
const TEST_ONLY_PUBLISHER_SEED: [u8; 32] = [0x0b; 32];
const WASM: &[u8] = b"\0asm\x0d\0\x01\0";

#[derive(Debug, Deserialize)]
struct Headers {
    #[serde(rename = "X-Hive-Manifest")]
    manifest: String,
    #[serde(rename = "X-Hive-Manifest-Signature")]
    manifest_signature: String,
    #[serde(rename = "X-Hive-Publisher-Certificate")]
    certificate: String,
    #[serde(rename = "X-Hive-Certificate-Signature")]
    certificate_signature: String,
}

#[derive(Debug, Deserialize)]
struct ChainVector {
    test_only_root_signing_key: String,
    root_public_key: String,
    test_only_publisher_signing_key: String,
    wasm_base64: String,
    #[serde(flatten)]
    chain: ModuleChain,
    headers: Headers,
}

fn manifest() -> SignedManifest {
    SignedManifest {
        capabilities: Capabilities {
            agent: true,
            chat: false,
            tools: vec!["echo".to_string(), "résumé".to_string()],
        },
        name: "vector-module".to_string(),
        sha256: hex::encode(Sha256::digest(WASM)),
        version: "1.2.3-rc.1".to_string(),
        wit_version: "0.2.0".to_string(),
    }
}

fn certificate(publisher: &SigningKey) -> PublisherCertificate {
    PublisherCertificate {
        not_before: 1_790_000_000,
        public_key: hex::encode(publisher.verifying_key().to_bytes()),
        publisher_id: "00000000-0000-4000-8000-000000000704".to_string(),
    }
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(format!("{VECTORS}/{name}")).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn chain_vector() -> ChainVector {
    serde_json::from_slice(&read("chain.json")).expect("chain.json parses")
}

#[test]
fn canonical_manifest_vector_is_stable() {
    let committed = read("canonical_manifest.json");
    let built = manifest().canonical_bytes();
    assert_eq!(
        built,
        committed,
        "canonical form changed; built:\n{}",
        String::from_utf8_lossy(&built)
    );
    let parsed: SignedManifest = serde_json::from_slice(&committed).unwrap();
    assert_eq!(parsed, manifest());
}

#[test]
fn chain_vector_is_reproduced_by_the_test_only_keys() {
    let vector = chain_vector();
    let root = SigningKey::from_bytes(&TEST_ONLY_ROOT_SEED);
    let publisher = SigningKey::from_bytes(&TEST_ONLY_PUBLISHER_SEED);
    assert_eq!(
        vector.test_only_root_signing_key,
        hex::encode(root.to_bytes())
    );
    assert_eq!(
        vector.test_only_publisher_signing_key,
        hex::encode(publisher.to_bytes())
    );
    assert_eq!(
        vector.root_public_key,
        hex::encode(root.verifying_key().to_bytes())
    );
    assert_eq!(BASE64.decode(&vector.wasm_base64).unwrap(), WASM);

    // Ed25519 is deterministic: the same keys over the same bytes give the
    // committed signatures.
    let certificate = certificate(&publisher).canonical_bytes();
    let manifest = manifest().canonical_bytes();
    let expected = ModuleChain {
        certificate: String::from_utf8(certificate.clone()).unwrap(),
        certificate_signature: BASE64.encode(root.sign(&certificate).to_bytes()),
        manifest: String::from_utf8(manifest.clone()).unwrap(),
        manifest_signature: BASE64.encode(publisher.sign(&manifest).to_bytes()),
    };
    assert_eq!(vector.chain, expected, "built: {expected:#?}");
    assert_eq!(
        vector.chain.manifest.as_bytes(),
        read("canonical_manifest.json")
    );

    let from_headers = ModuleChain::from_headers(
        Some(&vector.headers.manifest),
        Some(&vector.headers.manifest_signature),
        Some(&vector.headers.certificate),
        Some(&vector.headers.certificate_signature),
    )
    .unwrap();
    assert_eq!(from_headers, vector.chain);
}

#[test]
fn chain_vector_verifies_and_refuses_another_root() {
    let vector = chain_vector();
    let verified = verify_chain(&vector.root_public_key, &vector.chain, WASM).unwrap();
    assert_eq!(verified.manifest, manifest());
    assert_eq!(
        verify_download(
            &vector.root_public_key,
            &vector.chain,
            WASM,
            "vector-module",
            "1.2.3-rc.1"
        )
        .map(|v| v.manifest),
        Ok(manifest())
    );

    let other_root = hex::encode(
        SigningKey::from_bytes(&[0x0c; 32])
            .verifying_key()
            .to_bytes(),
    );
    assert_eq!(
        verify_chain(&other_root, &vector.chain, WASM),
        Err(VerifyError::CertificateSignatureMismatch)
    );
}
