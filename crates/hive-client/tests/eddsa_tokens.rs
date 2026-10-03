//! CX-0b (AGE-823): hive-client trusts the registry's EdDSA tokens only
//! through the root list, never HS256, and drives step-up and `External`
//! keys instead of `hive_` bearer keys. A wiremock double stands in for the
//! registry; `hive-e2e` runs the same flows against the real one.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use hive_client::session_key::{TokenError, certificate_message};
use hive_client::{
    ClientError, CreateExternalKey, ExternalKeyScope, HiveRegistryClient, HiveSession,
    StepUpAction, TokenPair,
};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, body_string, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn hex_of(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

fn certificate(root: &SigningKey, session: &SigningKey) -> Value {
    let public_key = hex_of(session);
    json!({
        "public_key": public_key,
        "signature": BASE64.encode(root.sign(&certificate_message(&public_key)).to_bytes()),
    })
}

fn jwt(alg: &str, claims: &Value, key: &SigningKey) -> String {
    let header = URL_SAFE_NO_PAD.encode(json!({ "alg": alg, "typ": "JWT" }).to_string());
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    let input = format!("{header}.{payload}");
    format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()).to_bytes())
    )
}

fn billing_claims(module: &str) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "sid": "s-1", "uid": "u-1", "mod": module, "ver": "1.0.0",
        "res": 100, "bal": 1000, "iat": now, "exp": now + 300,
    })
}

/// A registry double serving `cert` and answering acquire-session with
/// `token`.
async fn registry(cert: Value, token: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/hive-session-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(cert))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/credits/acquire-session"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "session_id": "s-1", "token": token, "balance_tokens": 900,
            "reserved_tokens": 100, "pricing_model": "paid",
        })))
        .mount(&server)
        .await;
    server
}

fn signed_in(server: &MockServer, root: &SigningKey) -> HiveRegistryClient {
    let base = server.uri();
    let session = std::sync::Arc::new(HiveSession::new(
        base.clone(),
        Some(TokenPair {
            token: "access".into(),
            expires_at: chrono::Utc::now() + chrono::TimeDelta::hours(1),
            refresh_token: "refresh".into(),
        }),
    ));
    HiveRegistryClient::new(&base)
        .with_local_root_key(&hex_of(root))
        .with_session(session)
}

#[tokio::test]
async fn a_billing_token_verifies_under_the_session_key_the_root_certifies() {
    let (root, session_key) = (key(1), key(2));
    let token = jwt("EdDSA", &billing_claims("paid-mod"), &session_key);
    let server = registry(certificate(&root, &session_key), &token).await;
    let client = signed_in(&server, &root);

    let session = client
        .acquire_session("paid-mod", "1.0.0", 100)
        .await
        .expect("a genuine billing token is accepted");
    assert_eq!(session.token, token);
    assert_eq!(
        client.session_key().await.unwrap(),
        session_key.verifying_key()
    );

    // A genuine token for another module is not handed over either.
    let err = client
        .acquire_session("other-mod", "1.0.0", 100)
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::Token(_)), "{err}");
}

#[tokio::test]
async fn hs256_and_uncertified_keys_are_refused() {
    let (root, session_key, impostor) = (key(1), key(2), key(3));

    // HS256 is gone: the token's `alg` alone refuses it.
    let hs256 = jwt("HS256", &billing_claims("m"), &session_key);
    let server = registry(certificate(&root, &session_key), &hs256).await;
    let err = signed_in(&server, &root)
        .acquire_session("m", "1.0.0", 100)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Token(TokenError::WrongAlgorithm(ref alg)) if alg == "HS256"),
        "{err}"
    );

    // A key the registry certifies itself (not the trusted root) is refused,
    // so a token it signed is never accepted.
    let token = jwt("EdDSA", &billing_claims("m"), &impostor);
    let server = registry(certificate(&impostor, &impostor), &token).await;
    let err = signed_in(&server, &root)
        .acquire_session("m", "1.0.0", 100)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Token(TokenError::UncertifiedKey)),
        "{err}"
    );

    // A token signed by another key under a genuine certificate.
    let server = registry(certificate(&root, &session_key), &token).await;
    let err = signed_in(&server, &root)
        .acquire_session("m", "1.0.0", 100)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ClientError::Token(TokenError::BadSignature)),
        "{err}"
    );
}

#[tokio::test]
async fn an_external_key_is_created_through_step_up_with_the_body_it_approved() {
    let root = key(1);
    let server = MockServer::start().await;
    let client = signed_in(&server, &root);
    let holder = key(9);
    let request = CreateExternalKey {
        name: "partner".into(),
        public_key: hex_of(&holder).to_uppercase(),
        monthly_limit_micros: 5_000_000,
        scope: vec![ExternalKeyScope {
            spec_id: "benford-analyst".into(),
            version: "1.0.0".into(),
        }],
        expires_in_days: None,
    };
    let id = uuid::Uuid::new_v4();
    let filed = |status: &str, assertion: Option<&str>| {
        json!({
            "id": id, "action": "api_key_create", "target": request.target(),
            "value": request.body(), "status": status,
            "expires_at": "2099-01-01T00:00:00Z",
            "page": format!("{}/step-up", server.uri()), "assertion": assertion,
        })
    };

    Mock::given(method("POST"))
        .and(path("/api/step-up/requests"))
        .and(header("authorization", "Bearer access"))
        .and(body_json(json!({
            "action": "api_key_create",
            "target": hex_of(&holder),
            "value": request.body(),
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(filed("pending", None)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/step-up/requests/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(filed("pending", None)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/step-up/requests/{id}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(filed("approved", Some("assertion-1"))),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/auth/api-keys"))
        .and(header("hive-step-up", "assertion-1"))
        .and(body_string(request.body()))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "id": uuid::Uuid::new_v4(), "name": "partner", "public_key": hex_of(&holder),
            "monthly_limit_micros": 5_000_000,
            "scope": [{ "spec_id": "benford-analyst", "version": "1.0.0" }],
            "created_at": "2026-10-03T00:00:00Z", "last_used_at": null,
            "expires_at": "2027-01-01T00:00:00Z", "revoked_at": null,
        })))
        .expect(1)
        .mount(&server)
        .await;

    let pending = client.request_external_key_step_up(&request).await.unwrap();
    assert_eq!(pending.status, "pending");
    assert!(pending.page.ends_with("/step-up"));
    let assertion = client
        .wait_for_step_up(pending.id, Duration::from_millis(10), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(assertion, "assertion-1");
    let created = client
        .create_external_key(&request, &assertion)
        .await
        .unwrap();
    assert_eq!(created.public_key, hex_of(&holder));
    assert_eq!(created.scope, request.scope);
}

#[tokio::test]
async fn an_expired_step_up_yields_no_assertion() {
    let root = key(1);
    let server = MockServer::start().await;
    let client = signed_in(&server, &root);
    let id = uuid::Uuid::new_v4();
    let answer = json!({
        "id": id, "action": "publisher_role", "target": "u", "value": "publisher",
        "status": "expired", "expires_at": "2000-01-01T00:00:00Z",
        "page": format!("{}/step-up", server.uri()),
    });
    Mock::given(method("POST"))
        .and(path("/api/step-up/requests"))
        .respond_with(ResponseTemplate::new(201).set_body_json(answer.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/step-up/requests/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer))
        .mount(&server)
        .await;

    let filed = client
        .request_step_up(StepUpAction::PublisherRole, "u", "publisher")
        .await
        .unwrap();
    let err = client
        .wait_for_step_up(filed.id, Duration::from_millis(10), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::StepUpNotApproved(_)), "{err}");
}
