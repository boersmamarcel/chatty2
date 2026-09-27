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

/// `GET /api/search` answers a `ModuleList`. No live recording exists: the
/// registry's search 500s on every query (found by the PL-E6 nightly,
/// proposed for PL-H7, AGE-610), so this replays the list recording, which
/// is the same type. Replace it with a real recording once search works.
#[tokio::test]
async fn search() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("q", "echo-agent"))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded("modules")))
        .expect(1)
        .mount(&server)
        .await;
    let list = anonymous(&server).search("echo-agent").await.unwrap();
    assert!(list.items.iter().any(|m| m.name == "echo-agent"));
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
    assert!(list.items.iter().any(|m| m.name == "echo-agent"));
}

#[tokio::test]
async fn get_module() {
    let server = MockServer::start().await;
    serve(&server, "GET", "/api/modules/echo-agent", "module").await;
    let module = anonymous(&server).get_module("echo-agent").await.unwrap();
    assert_eq!(module.name, "echo-agent");
    assert_eq!(module.latest_version.as_deref(), Some("0.1.0"));
    assert_eq!(module.execution_mode, "local");
}

#[tokio::test]
async fn list_versions() {
    let server = MockServer::start().await;
    serve(
        &server,
        "GET",
        "/api/modules/echo-agent/versions",
        "versions",
    )
    .await;
    let versions = anonymous(&server)
        .list_versions("echo-agent")
        .await
        .unwrap();
    let v = &versions.items[0];
    assert_eq!(
        (v.module_name.as_str(), v.version.as_str()),
        ("echo-agent", "0.1.0")
    );
    assert!(v.signature.is_some() && v.publisher_public_key.is_some());
}

#[tokio::test]
#[ignore = "known defect: PL-H7 (AGE-610)"]
async fn list_categories() {
    // The registry sends `slug`; hive-client's `Category` wants `name`.
    let server = MockServer::start().await;
    serve(&server, "GET", "/api/categories", "categories").await;
    let categories = anonymous(&server).list_categories().await.unwrap();
    assert!(!categories.items.is_empty());
}

// ── Download ──────────────────────────────────────────────────────────────

/// `GET /api/modules/{n}/{v}` with the integrity headers the registry was
/// recorded sending, over bytes signed here the way the registry signs, then
/// the versions list `finalize_download` reads the manifest from.
#[tokio::test]
async fn download() {
    let recorded_headers: Vec<String> =
        serde_json::from_value(recorded("download_headers")["headers"].clone()).unwrap();
    for needed in ["x-wasm-sha256", "x-signature", "x-publisher-public-key"] {
        assert!(
            recorded_headers.iter().any(|h| h == needed),
            "the registry no longer sends {needed}"
        );
    }

    let wasm = b"\0asm\x0d\0\x01\0 a component".to_vec();
    let key = SigningKey::from_bytes(&[9; 32]);
    let hash = hex::encode(Sha256::digest(&wasm));
    let signature =
        base64::engine::general_purpose::STANDARD.encode(key.sign(hash.as_bytes()).to_bytes());
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/modules/echo-agent/0.1.0"))
        .and(header("authorization", format!("Bearer {TOKEN}")))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-wasm-sha256", hash.as_str())
                .insert_header("x-signature", signature.as_str())
                .insert_header(
                    "x-publisher-public-key",
                    hex::encode(key.verifying_key().to_bytes()).as_str(),
                )
                .insert_header("x-trust-level", "signed")
                .set_body_bytes(wasm.clone()),
        )
        .expect(1)
        .mount(&server)
        .await;
    serve_authed(
        &server,
        "GET",
        "/api/modules/echo-agent/versions",
        "versions",
    )
    .await;

    let download = signed_in(&server)
        .download("echo-agent", "0.1.0")
        .await
        .unwrap();
    assert_eq!(download.wasm, wasm);
    assert_eq!(download.wasm_hash, hash);
    assert_eq!(download.trust_level, TrustLevel::Signed);
    assert_eq!(download.manifest["name"], "echo-agent");
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
            json!({ "events": [{ "idempotency_key": "k1", "module_name": "echo-agent" }] }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(recorded("usage_report")))
        .expect(1)
        .mount(&server)
        .await;
    let event = UsageEvent {
        idempotency_key: "k1".to_string(),
        module_name: "echo-agent".to_string(),
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
    let server = MockServer::start().await;
    serve_authed(
        &server,
        "POST",
        "/api/credits/acquire-session",
        "acquire_session",
    )
    .await;
    serve_authed(
        &server,
        "POST",
        "/api/credits/settle-session",
        "settle_session",
    )
    .await;
    let client = signed_in(&server);
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
