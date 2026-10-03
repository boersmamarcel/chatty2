//! Billing session token verification (CX-0b, AGE-823), on the host
//! target: EdDSA under a root-certified session key, never HS256.
//! Full end-to-end tests need a WASM runtime with the billing host imports.

#![cfg(not(target_arch = "wasm32"))]

use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use ed25519_dalek::{Signer, SigningKey};
use hive_billing_sdk::{verify_session_token, SESSION_KEY_CERT_DOMAIN};
use serde_json::{json, Value};

const NOW: i64 = 1_700_000_000;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn hex(key: &SigningKey) -> String {
    key.verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Claims as hive-registry mints them, with `session` certified by `root`.
fn claims(root: &SigningKey, session: &SigningKey) -> Value {
    let skey = hex(session);
    let mut message = SESSION_KEY_CERT_DOMAIN.to_vec();
    message.extend_from_slice(skey.as_bytes());
    json!({
        "sid": "s-1", "uid": "u-1", "mod": "paid-mod", "ver": "1.0.0",
        "res": 5000, "bal": 100000, "iat": NOW, "exp": NOW + 300,
        "skey": skey, "skey_sig": STANDARD.encode(root.sign(&message).to_bytes()),
    })
}

fn jwt(alg: &str, claims: &Value, signer: &SigningKey) -> String {
    let header = URL_SAFE_NO_PAD.encode(json!({ "alg": alg, "typ": "JWT" }).to_string());
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    let input = format!("{header}.{payload}");
    format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(signer.sign(input.as_bytes()).to_bytes())
    )
}

#[test]
fn a_token_signed_by_a_root_certified_key_verifies() {
    let (root, other_root, session) = (key(1), key(2), key(3));
    let token = jwt("EdDSA", &claims(&root, &session), &session);
    let verified = verify_session_token(&token, &[hex(&other_root), hex(&root)], NOW)
        .expect("a genuine token verifies");
    assert_eq!(verified.module_name, "paid-mod");
    assert_eq!(verified.res, 5000);
}

#[test]
fn hs256_is_refused() {
    let (root, session) = (key(1), key(3));
    let token = jwt("HS256", &claims(&root, &session), &session);
    let err = verify_session_token(&token, &[hex(&root)], NOW).unwrap_err();
    assert!(err.contains("not EdDSA"), "{err}");
}

#[test]
fn a_key_no_trusted_root_certified_is_refused() {
    let (root, impostor) = (key(1), key(4));
    // The impostor certifies itself: not a trusted root.
    let token = jwt("EdDSA", &claims(&impostor, &impostor), &impostor);
    let err = verify_session_token(&token, &[hex(&root)], NOW).unwrap_err();
    assert!(err.contains("not certified"), "{err}");
    // And with no root at all, nothing verifies.
    let err = verify_session_token(&token, &[], NOW).unwrap_err();
    assert!(err.contains("no registry root"), "{err}");
}

#[test]
fn a_token_signed_by_another_key_than_its_certified_one_is_refused() {
    let (root, session, other) = (key(1), key(3), key(5));
    let token = jwt("EdDSA", &claims(&root, &session), &other);
    let err = verify_session_token(&token, &[hex(&root)], NOW).unwrap_err();
    assert!(err.contains("signature verification failed"), "{err}");
}

#[test]
fn tampered_claims_are_refused() {
    let (root, session) = (key(1), key(3));
    let token = jwt("EdDSA", &claims(&root, &session), &session);
    let mut forged = claims(&root, &session);
    forged["res"] = json!(1);
    let parts: Vec<&str> = token.split('.').collect();
    let tampered = format!(
        "{}.{}.{}",
        parts[0],
        URL_SAFE_NO_PAD.encode(forged.to_string()),
        parts[2]
    );
    assert!(verify_session_token(&tampered, &[hex(&root)], NOW).is_err());
}

#[test]
fn an_expired_token_is_refused() {
    let (root, session) = (key(1), key(3));
    let token = jwt("EdDSA", &claims(&root, &session), &session);
    let err = verify_session_token(&token, &[hex(&root)], NOW + 301).unwrap_err();
    assert!(err.contains("expired"), "{err}");
}

#[test]
fn a_malformed_token_is_refused() {
    let root = key(1);
    assert!(verify_session_token("not.a-jwt", &[hex(&root)], NOW).is_err());
}
