//! hive-client ⟷ hive-registry HTTP contract (PL-E6, evaluation plan row
//! 5.1), per PR: every route the client calls, answered by wiremock with the
//! response recorded from a real registry (`tests/recorded/*.json`).
//!
//! The recordings come from the nightly's live stack: `hive-e2e`'s
//! `s5_01_recorded_responses_match_the_live_registry` fails when the live
//! registry's shape drifts from them and re-records them under
//! `HIVE_E2E_RECORD=1` (see `scripts/hive-e2e.sh`). Change a recording only
//! that way, never by hand.

use std::sync::Arc;

use base64::Engine as _;
use chrono::{TimeDelta, Utc};
use ed25519_dalek::{Signer, SigningKey};
use hive_client::models::{ListParams, UsageEvent};
use hive_client::verify::{
    Capabilities, HEADER_CERTIFICATE, HEADER_CERTIFICATE_SIGNATURE, HEADER_MANIFEST,
    HEADER_MANIFEST_SIGNATURE, PublisherCertificate, SignedManifest,
};
use hive_client::{HiveRegistryClient, HiveSession, TokenPair, TrustLevel};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "session-access-token";

fn recorded(name: &str) -> Value {
    let path = format!("{}/tests/recorded/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `method path` answers 200 with recording `name`, exactly once.
async fn serve(server: &MockServer, verb: &str, route: &str, name: &str) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded(name)))
        .expect(1)
        .mount(server)
        .await;
}

/// Like [`serve`], but only for the signed-in session's Bearer token.
async fn serve_authed(server: &MockServer, verb: &str, route: &str, name: &str) {
    Mock::given(method(verb))
        .and(path(route))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded(name)))
        .expect(1)
        .mount(server)
        .await;
}

fn anonymous(server: &MockServer) -> HiveRegistryClient {
    HiveRegistryClient::new(server.uri())
}

fn signed_in(server: &MockServer) -> HiveRegistryClient {
    let pair = TokenPair {
        token: TOKEN.to_string(),
        refresh_token: "session-refresh-token".to_string(),
        expires_at: Utc::now() + TimeDelta::hours(1),
    };
    let session = Arc::new(HiveSession::new(server.uri(), Some(pair)));
    HiveRegistryClient::new(server.uri()).with_session(session)
}

// ── Auth ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn register_login_refresh_and_logout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/auth/register"))
        .and(body_partial_json(
            json!({ "username": "alice", "email": "a@example.com", "password": "pw-twelve-chars" }),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(recorded("auth_register")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/auth/login"))
        .and(body_partial_json(
            json!({ "email": "a@example.com", "password": "pw-twelve-chars" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded("auth_login")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/auth/refresh"))
        .and(body_partial_json(
            json!({ "refresh_token": "recorded-refresh-token" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded("auth_refresh")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/auth/logout"))
        .and(body_partial_json(
            json!({ "refresh_token": "recorded-refresh-token" }),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let client = anonymous(&server);
    let registered = client
        .register("alice", "a@example.com", "pw-twelve-chars")
        .await
        .unwrap();
    assert_eq!(registered.token, "recorded-access-token");
    assert_eq!(registered.refresh_token, "recorded-refresh-token");
    let login = client
        .login("a@example.com", "pw-twelve-chars")
        .await
        .unwrap();
    let refreshed = client.refresh(&login.refresh_token).await.unwrap();
    assert!(refreshed.expires_at > login.expires_at - TimeDelta::hours(1));
    client.logout(&refreshed.refresh_token).await.unwrap();
}

// ── Browse ────────────────────────────────────────────────────────────────

/// `GET /api/search` answers a `ModuleList` (PL-H7, AGE-610, fixed the 500
/// every query used to get).
#[tokio::test]
async fn search() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("q", "echo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded("search")))
        .expect(1)
        .mount(&server)
        .await;
    let list = anonymous(&server).search("echo").await.unwrap();
    assert!(list.items.iter().any(|m| m.name == "echo"));
}

#[tokio::test]
async fn list_modules() {
    let server = MockServer::start().await;
    serve(&server, "GET", "/api/modules", "modules").await;
    let list = anonymous(&server)
        .list_modules(&ListParams::default())
        .await
        .unwrap();
    assert_eq!(list.items.len() as i64, list.total.min(list.per_page));
    assert!(list.items.iter().any(|m| m.name == "echo"));
}

#[tokio::test]
async fn get_module() {
    let server = MockServer::start().await;
    serve(&server, "GET", "/api/modules/echo", "module").await;
    let module = anonymous(&server).get_module("echo").await.unwrap();
    assert_eq!(module.name, "echo");
    assert_eq!(module.latest_version.as_deref(), Some("0.1.0"));
    assert_eq!(module.execution_mode, "local");
    // The single-module endpoint embeds the latest version's stored
    // manifest (API spec §6.3); the list/search endpoints do not.
    assert!(module.manifest.is_some());
}

#[tokio::test]
async fn list_versions() {
    let server = MockServer::start().await;
    serve(&server, "GET", "/api/modules/echo/versions", "versions").await;
    let versions = anonymous(&server).list_versions("echo").await.unwrap();
    let v = &versions.items[0];
    assert_eq!(
        (v.module_name.as_str(), v.version.as_str()),
        ("echo", "0.1.0")
    );
    assert!(v.signature.is_some() && v.publisher_public_key.is_some());
}

#[tokio::test]
async fn list_categories() {
    let server = MockServer::start().await;
    serve(&server, "GET", "/api/categories", "categories").await;
    let categories = anonymous(&server).list_categories().await.unwrap();
    assert!(!categories.items.is_empty());
}

// ── Download ──────────────────────────────────────────────────────────────

/// The four `X-Hive-*` chain headers for `wasm` published as `name@version`,
/// certified by the root `root_seed` for the publisher `[0x0b; 32]`, the way
/// hive-registry signs (AGE-704).
fn chain_headers(root_seed: u8, name: &str, version: &str, wasm: &[u8]) -> Vec<(String, String)> {
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let root = SigningKey::from_bytes(&[root_seed; 32]);
    let publisher = SigningKey::from_bytes(&[0x0b; 32]);
    let certificate = PublisherCertificate {
        not_before: 1_790_000_000,
        public_key: hex::encode(publisher.verifying_key().to_bytes()),
        publisher_id: "00000000-0000-4000-8000-000000000001".to_string(),
    }
    .canonical_bytes();
    let manifest = SignedManifest {
        capabilities: Capabilities::default(),
        name: name.to_string(),
        sha256: hex::encode(Sha256::digest(wasm)),
        version: version.to_string(),
        wit_version: "0.3.0".to_string(),
    }
    .canonical_bytes();
    vec![
        (HEADER_MANIFEST.to_string(), b64(&manifest)),
        (
            HEADER_MANIFEST_SIGNATURE.to_string(),
            b64(&publisher.sign(&manifest).to_bytes()),
        ),
        (HEADER_CERTIFICATE.to_string(), b64(&certificate)),
        (
            HEADER_CERTIFICATE_SIGNATURE.to_string(),
            b64(&root.sign(&certificate).to_bytes()),
        ),
    ]
}

/// The TEST-ONLY root the downloads below are certified by.
const ROOT_SEED: u8 = 0x0a;

fn root_hex(seed: u8) -> String {
    hex::encode(
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

/// Serve `echo@0.1.0` as `wasm` under `headers`.
async fn serve_download(server: &MockServer, wasm: &[u8], headers: Vec<(String, String)>) {
    let mut response = ResponseTemplate::new(200)
        .insert_header("x-trust-level", "signed")
        .set_body_bytes(wasm.to_vec());
    for (name, value) in headers {
        response = response.insert_header(name.as_str(), value.as_str());
    }
    Mock::given(method("GET"))
        .and(path("/api/modules/echo/0.1.0"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(response)
        .mount(server)
        .await;
}

const WASM: &[u8] = b"\0asm\x0d\0\x01\0 a component";

/// `GET /api/modules/{n}/{v}` with the signing-chain headers the registry was
/// recorded sending, over bytes signed here the way the registry signs, then
/// the versions list `finalize_download` reads the manifest from.
#[tokio::test]
async fn download() {
    let recorded_headers: Vec<String> =
        serde_json::from_value(recorded("download_headers")["headers"].clone()).unwrap();
    for needed in [
        HEADER_MANIFEST,
        HEADER_MANIFEST_SIGNATURE,
        HEADER_CERTIFICATE,
        HEADER_CERTIFICATE_SIGNATURE,
    ] {
        assert!(
            recorded_headers
                .iter()
                .any(|h| h.eq_ignore_ascii_case(needed)),
            "the registry no longer sends {needed}"
        );
    }

    let server = MockServer::start().await;
    serve_download(
        &server,
        WASM,
        chain_headers(ROOT_SEED, "echo", "0.1.0", WASM),
    )
    .await;
    serve_authed(&server, "GET", "/api/modules/echo/versions", "versions").await;

    let download = signed_in(&server)
        .with_local_root_key(&root_hex(ROOT_SEED))
        .download("echo", "0.1.0")
        .await
        .unwrap();
    assert_eq!(download.wasm, WASM);
    assert_eq!(download.wasm_hash, hex::encode(Sha256::digest(WASM)));
    assert_eq!(download.trust_level, TrustLevel::Signed);
    assert_eq!(
        download.publisher_public_key,
        hex::encode(
            SigningKey::from_bytes(&[0x0b; 32])
                .verifying_key()
                .to_bytes()
        )
    );
    assert_eq!(download.signed_manifest.name, "echo");
    assert_eq!(download.manifest["name"], "echo");
}

/// No root key for the registry: refused before any request (no TOFU).
#[tokio::test]
async fn download_without_a_trusted_root_is_refused_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let client = signed_in(&server);
    assert!(client.root_keys().is_empty(), "CHATTY_HIVE_ROOT_KEY is set");
    let err = client.download("echo", "0.1.0").await.unwrap_err();
    assert!(
        matches!(err, hive_client::ClientError::NoTrustedRoot { .. }),
        "{err}"
    );
}

/// A download missing any of the four chain headers is refused.
#[tokio::test]
async fn download_without_the_chain_headers_is_refused() {
    for dropped in 0..4 {
        let server = MockServer::start().await;
        let mut headers = chain_headers(ROOT_SEED, "echo", "0.1.0", WASM);
        let (missing, _) = headers.remove(dropped);
        serve_download(&server, WASM, headers).await;
        let err = signed_in(&server)
            .with_local_root_key(&root_hex(ROOT_SEED))
            .download("echo", "0.1.0")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains(&format!("missing {missing}")),
            "{err}"
        );
    }
}

/// A self-consistent chain under a root this client does not trust, and a
/// genuine chain for another module, are both refused.
#[tokio::test]
async fn download_certified_by_another_root_or_for_another_module_is_refused() {
    for (headers, why) in [
        (
            chain_headers(0x42, "echo", "0.1.0", WASM),
            "not signed by the registry root key",
        ),
        (
            chain_headers(ROOT_SEED, "spin", "0.1.0", WASM),
            "is for spin@0.1.0, not the requested echo@0.1.0",
        ),
    ] {
        let server = MockServer::start().await;
        serve_download(&server, WASM, headers).await;
        let err = signed_in(&server)
            .with_local_root_key(&root_hex(ROOT_SEED))
            .download("echo", "0.1.0")
            .await
            .unwrap_err();
        assert!(err.to_string().contains(why), "{err}");
    }
}

// ── Credits, usage, billing sessions ──────────────────────────────────────

#[tokio::test]
async fn credit_balance() {
    let server = MockServer::start().await;
    serve_authed(&server, "GET", "/api/credits/balance", "credits_balance").await;
    let balance = signed_in(&server).get_credit_balance().await.unwrap();
    assert_eq!(balance.balance_tokens, 0);
}

#[tokio::test]
async fn module_pricing() {
    let server = MockServer::start().await;
    serve(
        &server,
        "GET",
        "/api/modules/paid-module/pricing",
        "module_pricing",
    )
    .await;
    let pricing = anonymous(&server)
        .get_module_pricing("paid-module")
        .await
        .unwrap();
    assert_eq!(pricing.module_name, "paid-module");
    assert_eq!(pricing.price_per_call, "0.0100");
}

#[tokio::test]
async fn report_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/usage/report"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .and(body_partial_json(
            json!({ "events": [{ "idempotency_key": "k1", "module_name": "echo" }] }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded("usage_report")))
        .expect(1)
        .mount(&server)
        .await;
    let event = UsageEvent {
        idempotency_key: "k1".to_string(),
        module_name: "echo".to_string(),
        module_version: "0.1.0".to_string(),
        event_type: "invocation".to_string(),
        input_tokens: Some(3),
        output_tokens: Some(2),
        fuel_consumed: None,
        execution_ms: Some(1),
        metadata: None,
        occurred_at: Utc::now(),
    };
    let response = signed_in(&server).report_usage(vec![event]).await.unwrap();
    assert_eq!((response.accepted, response.duplicates), (1, 0));
}

#[tokio::test]
async fn acquire_and_settle_a_billing_session() {
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD};
    use ed25519_dalek::Signer;

    let server = MockServer::start().await;
    // The recorded token is scrubbed; the client now verifies it (CX-0b),
    // so the double serves a real EdDSA one under a root-certified key.
    let (root, session_key) = (
        ed25519_dalek::SigningKey::from_bytes(&[1; 32]),
        ed25519_dalek::SigningKey::from_bytes(&[2; 32]),
    );
    let session_hex = hex::encode(session_key.verifying_key().to_bytes());
    Mock::given(method("GET"))
        .and(path("/.well-known/hive-session-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "public_key": session_hex,
            "signature": BASE64.encode(
                root.sign(&hive_client::session_key::certificate_message(&session_hex))
                    .to_bytes()
            ),
        })))
        .mount(&server)
        .await;
    let now = Utc::now().timestamp();
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"EdDSA","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(
            json!({
                "sid": "e96ad058-bbba-47a4-9f8f-738b85e97861", "uid": "u",
                "mod": "paid-module", "ver": "1.0.0", "res": 100, "bal": 0,
                "iat": now, "exp": now + 300,
            })
            .to_string()
        )
    );
    let token = format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(session_key.sign(input.as_bytes()).to_bytes())
    );
    let mut acquired = recorded("acquire_session");
    acquired["token"] = json!(token);
    Mock::given(method("POST"))
        .and(path("/api/credits/acquire-session"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(acquired))
        .expect(1)
        .mount(&server)
        .await;
    serve_authed(
        &server,
        "POST",
        "/api/credits/settle-session",
        "settle_session",
    )
    .await;
    let client =
        signed_in(&server).with_local_root_key(&hex::encode(root.verifying_key().to_bytes()));
    let session = client
        .acquire_session("paid-module", "1.0.0", 100)
        .await
        .unwrap();
    assert_eq!(session.reserved_tokens, 100);
    let settled = client
        .settle_session(&session.session_id, 1, 1)
        .await
        .unwrap();
    assert_eq!(settled.session_id, session.session_id);
    assert_eq!(
        settled.tokens_deducted + settled.tokens_released,
        session.reserved_tokens
    );
}
