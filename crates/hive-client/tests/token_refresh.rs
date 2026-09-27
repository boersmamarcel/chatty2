//! The Hive session keeps a one-hour access token alive (AGE-609): it
//! refreshes before expiry and after a 401, shares one refresh between
//! concurrent callers, signs out when the refresh token is rejected, and
//! usage events survive a 401.

use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use hive_client::{
    HiveRegistryClient, HiveSession, SessionState, TokenPair, UsageCollector, UsageCollectorConfig,
};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn pair(token: &str, refresh_token: &str, expires_in: TimeDelta) -> TokenPair {
    TokenPair {
        token: token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at: Utc::now() + expires_in,
    }
}

fn pair_json(token: &str, refresh_token: &str) -> Value {
    json!({
        "token": token,
        "refresh_token": refresh_token,
        "expires_at": (Utc::now() + TimeDelta::hours(1)).to_rfc3339(),
    })
}

fn balance() -> Value {
    json!({ "balance_tokens": 7, "lifetime_purchased_tokens": 7, "lifetime_consumed_tokens": 0 })
}

async fn mock_balance(server: &MockServer, token: &str, status: u16) {
    let response = if status == 200 {
        ResponseTemplate::new(200).set_body_json(balance())
    } else {
        ResponseTemplate::new(status)
    };
    Mock::given(method("GET"))
        .and(path("/api/credits/balance"))
        .and(header("authorization", format!("Bearer {token}")))
        .respond_with(response)
        .mount(server)
        .await;
}

/// `POST /api/auth/refresh` answering `old_refresh` with the pair
/// (`token`, `refresh_token`), expected exactly `times` times.
async fn mock_refresh(
    server: &MockServer,
    old_refresh: &str,
    token: &str,
    refresh_token: &str,
    times: u64,
) {
    Mock::given(method("POST"))
        .and(path("/api/auth/refresh"))
        .and(body_json(json!({ "refresh_token": old_refresh })))
        .respond_with(ResponseTemplate::new(200).set_body_json(pair_json(token, refresh_token)))
        .expect(times)
        .mount(server)
        .await;
}

fn client(server: &MockServer, session: &Arc<HiveSession>) -> HiveRegistryClient {
    HiveRegistryClient::new(server.uri()).with_session(Arc::clone(session))
}

#[tokio::test]
async fn login_returns_the_refresh_token_and_expiry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pair_json("access", "refresh")))
        .mount(&server)
        .await;

    let pair = HiveRegistryClient::new(server.uri())
        .login("a@example.com", "pw")
        .await
        .unwrap();

    assert_eq!(pair.token, "access");
    assert_eq!(pair.refresh_token, "refresh");
    assert!(pair.expires_at > Utc::now() + TimeDelta::minutes(59));
    let debug = format!("{pair:?}");
    assert!(
        !debug.contains("\"access\"") && !debug.contains("\"refresh\""),
        "{debug}"
    );
}

#[tokio::test]
async fn refreshes_an_access_token_about_to_expire_before_using_it() {
    let server = MockServer::start().await;
    mock_refresh(&server, "r1", "new", "r2", 1).await;
    mock_balance(&server, "new", 200).await;
    let session = Arc::new(HiveSession::new(
        server.uri(),
        Some(pair("old", "r1", TimeDelta::seconds(30))),
    ));

    let balance = client(&server, &session)
        .get_credit_balance()
        .await
        .unwrap();

    assert_eq!(balance.balance_tokens, 7);
    let state = session.subscribe().borrow().clone();
    let SessionState::SignedIn(current) = state else {
        panic!("still signed in");
    };
    assert_eq!(
        (current.token.as_str(), current.refresh_token.as_str()),
        ("new", "r2")
    );
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries_once() {
    let server = MockServer::start().await;
    mock_balance(&server, "old", 401).await;
    mock_balance(&server, "new", 200).await;
    mock_refresh(&server, "r1", "new", "r2", 1).await;
    let session = Arc::new(HiveSession::new(
        server.uri(),
        Some(pair("old", "r1", TimeDelta::hours(1))),
    ));

    let balance = client(&server, &session)
        .get_credit_balance()
        .await
        .unwrap();

    assert_eq!(balance.balance_tokens, 7);
}

#[tokio::test]
async fn two_concurrent_401s_share_one_refresh() {
    let server = MockServer::start().await;
    // Hold the 401s so both calls have sent the old token before either
    // refreshes.
    Mock::given(method("GET"))
        .and(path("/api/credits/balance"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401).set_delay(std::time::Duration::from_millis(200)))
        .expect(2)
        .mount(&server)
        .await;
    mock_balance(&server, "new", 200).await;
    mock_refresh(&server, "r1", "new", "r2", 1).await;
    let session = Arc::new(HiveSession::new(
        server.uri(),
        Some(pair("old", "r1", TimeDelta::hours(1))),
    ));
    let (a, b) = (client(&server, &session), client(&server, &session));

    let (a, b) = tokio::join!(a.get_credit_balance(), b.get_credit_balance());

    assert_eq!(a.unwrap().balance_tokens, 7);
    assert_eq!(b.unwrap().balance_tokens, 7);
    // `expect(1)` on the refresh mock is verified when `server` drops.
}

#[tokio::test]
async fn a_rotated_refresh_token_signs_out_without_looping() {
    let server = MockServer::start().await;
    mock_balance(&server, "old", 401).await;
    Mock::given(method("POST"))
        .and(path("/api/auth/refresh"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let session = Arc::new(HiveSession::new(
        server.uri(),
        Some(pair("old", "r1", TimeDelta::hours(1))),
    ));
    let states = session.subscribe();
    let client = client(&server, &session);

    let first = client.get_credit_balance().await;
    // Signed out now: a second call goes out without a token and does not
    // try to refresh again.
    let second = client.get_credit_balance().await;

    assert!(matches!(
        first,
        Err(hive_client::ClientError::Http { status: 401, .. })
    ));
    assert!(second.is_err());
    assert_eq!(*states.borrow(), SessionState::Revoked);
    assert_eq!(session.access_token().await, None);
}

#[tokio::test]
async fn usage_events_survive_a_401() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/usage/report"))
        .and(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/auth/refresh"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/usage/report"))
        .and(header("authorization", "Bearer new"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "accepted": 1, "duplicates": 0 })),
        )
        .expect(1)
        .mount(&server)
        .await;
    let queue_dir = tempfile::tempdir().unwrap();
    let session = Arc::new(HiveSession::new(
        server.uri(),
        Some(pair("old", "r1", TimeDelta::hours(1))),
    ));
    let collector = UsageCollector::new(
        server.uri(),
        UsageCollectorConfig {
            queue_dir: queue_dir.path().to_path_buf(),
            ..Default::default()
        },
    );
    collector.set_session(Arc::clone(&session)).await;
    collector
        .record_invocation("m", "1.0.0", Some(1), Some(2), None, None)
        .await;

    assert!(
        collector.flush().await.is_err(),
        "401: the user is signed out"
    );
    session
        .sign_in(pair("new", "r2", TimeDelta::hours(1)))
        .await;
    let report = collector.flush().await.unwrap();

    assert_eq!(report.accepted, 1);
    let sent = server.received_requests().await.unwrap();
    let last: Value = serde_json::from_slice(&sent.last().unwrap().body).unwrap();
    assert_eq!(last["events"].as_array().unwrap().len(), 1);
    assert_eq!(last["events"][0]["module_name"], "m");
}
