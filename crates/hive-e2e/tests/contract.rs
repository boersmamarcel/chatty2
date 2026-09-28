//! S5 · Hive contract (evaluation plan §3 S5, rows 5.1–5.10): chatty's
//! client, installer and gateway against a live hive stack.
//!
//! Every test asserts the correct behaviour. Rows red today fail with a
//! message naming the finding and the PL-H issue that fixes it.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{TimeDelta, Utc};
use hive_client::models::{ListParams, UsageEvent};
use hive_client::{
    HiveRegistryClient, HiveSession, TokenPair, UsageCollector, UsageCollectorConfig,
};
use hive_e2e::{
    SEEDED_VERSION, Stack, flat_manifest, install_from_hive, local_module_registry,
    mint_session_token, module_dir, send, send_via_gateway, start_gateway, unique,
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

/// The fixture the seed script publishes and most rows download.
const ECHO: &str = "echo";

// ── 5.1 ───────────────────────────────────────────────────────────────────

/// 5.1: every route hive-client calls answers something it deserializes.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_01_hive_client_deserializes_every_registry_route() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let paid = unique("paid-e2e");
    stack
        .publish_ok(
            &publisher,
            &flat_manifest(&paid, "1.0.0", ""),
            stack.fixture(ECHO),
        )
        .await;
    stack.set_pricing(&publisher, &paid, 0.01).await;

    let mut breaks: Vec<String> = Vec::new();

    // Auth, through hive-client itself.
    let anon = HiveRegistryClient::new(&stack.registry);
    let username = unique("e2e-client");
    let email = format!("{username}@example.com");
    let password = "e2e-client-password";
    let registered = anon.register(&username, &email, password).await;
    note(
        &mut breaks,
        "register (POST /api/auth/register)",
        registered,
    );
    let login = anon.login(&email, password).await;
    let pair = note(&mut breaks, "login (POST /api/auth/login)", login);
    let pair = match pair {
        Some(pair) => note(
            &mut breaks,
            "refresh (POST /api/auth/refresh)",
            anon.refresh(&pair.refresh_token).await,
        ),
        None => None,
    };
    if let Ok(spare) = anon.login(&email, password).await {
        let logout = anon.logout(&spare.refresh_token).await;
        note(&mut breaks, "logout (POST /api/auth/logout)", logout);
    }
    let Some(pair) = pair else {
        panic!("cannot sign in through hive-client:\n{}", breaks.join("\n"));
    };

    let client = HiveRegistryClient::new(&stack.registry)
        .with_session(Arc::new(HiveSession::new(&stack.registry, Some(pair))));
    let r = client.search(ECHO).await;
    note(&mut breaks, "search (GET /api/search)", r);
    let r = client.list_modules(&ListParams::default()).await;
    note(&mut breaks, "list_modules (GET /api/modules)", r);
    let r = client.get_module(ECHO).await;
    note(&mut breaks, "get_module (GET /api/modules/{n})", r);
    let r = client.list_versions(ECHO).await;
    note(
        &mut breaks,
        "list_versions (GET /api/modules/{n}/versions)",
        r,
    );
    let r = client.list_categories().await;
    note(&mut breaks, "list_categories (GET /api/categories)", r);
    let r = client.download(ECHO, SEEDED_VERSION).await;
    note(&mut breaks, "download (GET /api/modules/{n}/{v})", r);
    let r = client.get_credit_balance().await;
    note(
        &mut breaks,
        "get_credit_balance (GET /api/credits/balance)",
        r,
    );
    let r = client.get_module_pricing(&paid).await;
    note(
        &mut breaks,
        "get_module_pricing (GET /api/modules/{n}/pricing)",
        r,
    );
    let r = client
        .report_usage(vec![usage_event(ECHO, "invocation")])
        .await;
    note(&mut breaks, "report_usage (POST /api/usage/report)", r);
    let r = client.acquire_session(&paid, "1.0.0", 100).await;
    if let Some(session) = note(
        &mut breaks,
        "acquire_session (POST /api/credits/acquire-session)",
        r,
    ) {
        let r = client.settle_session(&session.session_id, 1, 1).await;
        note(
            &mut breaks,
            "settle_session (POST /api/credits/settle-session)",
            r,
        );
    }

    assert!(
        breaks.is_empty(),
        "hive-client and hive-registry disagree on {} route(s):\n{}",
        breaks.len(),
        breaks.join("\n")
    );
}

/// The value, or `None` with the route's error noted in `breaks`.
fn note<T>(
    breaks: &mut Vec<String>,
    route: &str,
    result: Result<T, hive_client::ClientError>,
) -> Option<T> {
    result
        .map_err(|e| breaks.push(format!("{route}: {e}")))
        .ok()
}

/// 5.1: the recorded registry responses `hive-client`'s per-PR contract
/// tests (`crates/hive-client/tests/registry_contract.rs`) replay still have
/// the live registry's shape: the same JSON key paths, the same download
/// headers. `HIVE_E2E_RECORD=1` re-records them instead.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_01_recorded_responses_match_the_live_registry() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let paid = unique("paid-e2e");
    stack
        .publish_ok(
            &publisher,
            &flat_manifest(&paid, "1.0.0", ""),
            stack.fixture(ECHO),
        )
        .await;
    stack.set_pricing(&publisher, &paid, 0.01).await;

    let user = unique("e2e-rec");
    let email = format!("{user}@example.com");
    let password = "e2e-record-password";
    let mut live: Vec<(&str, StatusCode, Value)> = Vec::new();

    let (status, register) = send(
        stack
            .request(Method::POST, "/auth/register")
            .json(&json!({ "username": user, "email": email, "password": password })),
    )
    .await;
    live.push(("auth_register", status, register));
    let (status, login) = send(
        stack
            .request(Method::POST, "/auth/login")
            .json(&json!({ "email": email, "password": password })),
    )
    .await;
    let refresh_token = login["refresh_token"]
        .as_str()
        .expect("a refresh token")
        .to_string();
    live.push(("auth_login", status, login));
    let (status, refresh) = send(
        stack
            .request(Method::POST, "/auth/refresh")
            .json(&json!({ "refresh_token": refresh_token })),
    )
    .await;
    let token = refresh["token"]
        .as_str()
        .expect("an access token")
        .to_string();
    live.push(("auth_refresh", status, refresh));

    let get = |path: String| {
        let request = stack.request(Method::GET, &path).bearer_auth(&token);
        async move { send(request).await }
    };
    let mut push = |name, (status, body)| live.push((name, status, body));
    push("search", get(format!("/search?q={ECHO}")).await);
    push("modules", get("/modules".into()).await);
    push("module", get(format!("/modules/{ECHO}")).await);
    push("versions", get(format!("/modules/{ECHO}/versions")).await);
    push("categories", get("/categories".into()).await);
    push("credits_balance", get("/credits/balance".into()).await);
    push(
        "module_pricing",
        get(format!("/modules/{paid}/pricing")).await,
    );

    let download = stack
        .request(Method::GET, &format!("/modules/{ECHO}/{SEEDED_VERSION}"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("a download");
    let headers: Vec<String> = download
        .headers()
        .keys()
        .map(|k| k.as_str().to_string())
        .filter(|k| k.starts_with("x-") && !k.starts_with("x-ratelimit"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    push(
        "download_headers",
        (download.status(), json!({ "headers": headers })),
    );

    let post = |path: &str, body: Value| {
        let request = stack
            .request(Method::POST, path)
            .bearer_auth(&token)
            .json(&body);
        async move { send(request).await }
    };
    let event = usage_event(ECHO, "invocation");
    push(
        "usage_report",
        post("/usage/report", json!({ "events": [event] })).await,
    );
    let (status, acquired) = post(
        "/credits/acquire-session",
        json!({ "module_name": paid, "module_version": "1.0.0", "estimated_tokens": 100 }),
    )
    .await;
    let session_id = acquired["session_id"]
        .as_str()
        .expect("a session id")
        .to_string();
    push("acquire_session", (status, acquired));
    push(
        "settle_session",
        post(
            "/credits/settle-session",
            json!({ "session_id": session_id, "input_tokens": 1, "output_tokens": 1 }),
        )
        .await,
    );

    // A route that fails live has nothing to record or compare: that is a
    // break in itself.
    let mut drift: Vec<String> = live
        .iter()
        .filter(|(_, status, _)| !status.is_success())
        .map(|(name, status, body)| format!("{name}: the registry answered {status}: {body}"))
        .collect();
    live.retain(|(_, status, _)| status.is_success());

    let dir = recorded_dir();
    if std::env::var_os("HIVE_E2E_RECORD").is_some() {
        std::fs::create_dir_all(&dir).expect("the recorded dir");
        for (name, _, value) in &live {
            let scrubbed = scrub(value, &[(&paid, "paid-module"), (&user, "recorded-user")]);
            let text = serde_json::to_string_pretty(&scrubbed).expect("JSON") + "\n";
            std::fs::write(dir.join(format!("{name}.json")), text).expect("write a recording");
        }
        assert!(drift.is_empty(), "not recorded:\n{}", drift.join("\n"));
        return;
    }

    for (name, _, value) in &live {
        let path = dir.join(format!("{name}.json"));
        let recorded: Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
        )
        .expect("recorded JSON");
        let (live_paths, recorded_paths) = (key_paths(value), key_paths(&recorded));
        let (added, removed): (Vec<_>, Vec<_>) = (
            live_paths.difference(&recorded_paths).collect(),
            recorded_paths.difference(&live_paths).collect(),
        );
        if name == &"download_headers" && value != &recorded {
            drift.push(format!("{name}: live {value} vs recorded {recorded}"));
        } else if !added.is_empty() || !removed.is_empty() {
            drift.push(format!("{name}: live adds {added:?}, drops {removed:?}"));
        }
    }
    assert!(
        drift.is_empty(),
        "the registry's responses drifted from crates/hive-client/tests/recorded \
         (re-record with HIVE_E2E_RECORD=1, then fix hive-client):\n{}",
        drift.join("\n")
    );
}

fn recorded_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hive-client/tests/recorded")
}

/// Tokens become placeholders and run-specific names stable ones, so a
/// re-recording diffs only where the registry changed.
fn scrub(value: &Value, names: &[(&str, &str)]) -> Value {
    fn walk(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, field) in map.iter_mut() {
                    match (key.as_str(), field.is_string()) {
                        ("token", true) => *field = json!("recorded-access-token"),
                        ("refresh_token", true) => *field = json!("recorded-refresh-token"),
                        _ => walk(field),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut text = value.to_string();
    for (from, to) in names {
        text = text.replace(from, to);
    }
    let mut value: Value = serde_json::from_str(&text).expect("still JSON");
    walk(&mut value);
    value
}

/// Every object key path in `value` (`.items[].name`), array elements
/// merged. Values are not compared: a recording is a shape, not a snapshot.
fn key_paths(value: &Value) -> BTreeSet<String> {
    fn walk(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, field) in map {
                    let path = format!("{prefix}.{key}");
                    out.insert(path.clone());
                    walk(field, &path, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, &format!("{prefix}[]"), out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeSet::new();
    walk(value, "", &mut out);
    out
}

fn usage_event(module: &str, event_type: &str) -> UsageEvent {
    UsageEvent {
        idempotency_key: uuid_key(),
        module_name: module.to_string(),
        module_version: SEEDED_VERSION.to_string(),
        event_type: event_type.to_string(),
        input_tokens: Some(3),
        output_tokens: Some(2),
        fuel_consumed: None,
        execution_ms: Some(1),
        metadata: None,
        occurred_at: Utc::now(),
    }
}

fn uuid_key() -> String {
    unique("e2e-usage")
}

// ── 5.2 ───────────────────────────────────────────────────────────────────

/// 5.2: a module published with `[capabilities]`, `[protocols]` and
/// `[resources]` is installed with them.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_02_published_manifest_sections_reach_the_installed_module() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let name = unique("caps-e2e");
    let sections = "\n[capabilities]\ntools = [\"echo\", \"reverse\", \"count_words\"]\n\
                    \n[protocols]\nmcp = true\n\
                    \n[resources]\nmax_memory_mb = 32\nmax_execution_ms = 5000\n";
    stack
        .publish_ok(
            &publisher,
            &flat_manifest(&name, "1.0.0", sections),
            stack.fixture(ECHO),
        )
        .await;

    let client = publisher.client(&stack.registry);
    let meta = client
        .get_module(&name)
        .await
        .expect("the module's metadata");
    install_from_hive(&client, &meta, "1.0.0")
        .await
        .expect("the install succeeds");

    let path = module_dir().join(&name).join("module.toml");
    let text = std::fs::read_to_string(&path).expect("the installed module.toml");
    let installed: toml::Table = text.parse().expect("valid TOML");
    let section = |key: &str| {
        installed
            .get(key)
            .cloned()
            .unwrap_or(toml::Value::Table(Default::default()))
    };
    let expected: toml::Table = sections.parse().expect("valid TOML");
    let missing: Vec<String> = ["capabilities", "protocols", "resources"]
        .into_iter()
        .filter(|key| section(key) != expected[*key])
        .map(|key| {
            format!(
                "[{key}]: published {}, installed {}",
                expected[key],
                section(key)
            )
        })
        .collect();
    assert!(
        missing.is_empty(),
        "the published manifest's sections did not reach the installed \
         module.toml:\n{}\n--- installed {} ---\n{text}",
        missing.join("\n"),
        path.display()
    );
}

// ── 5.3 ───────────────────────────────────────────────────────────────────

/// 5.3: past its one-hour access token, the desktop client refreshes and
/// keeps working — download and usage report (the plan's "run" was the
/// runner path, which PL-H8 retires). Two ways a token is past expiry: the
/// session knows (`expires_at` passed → refresh first) and it does not
/// (clock skew → 401 → refresh → retry). F7, fixed by PL-H6 (AGE-609).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_03_expired_access_token_is_refreshed_and_usage_survives() {
    let stack = Stack::from_env();
    let expired_at = Utc::now() - TimeDelta::minutes(10);
    for (case, believed_expiry) in [
        ("session knows the token expired", expired_at),
        (
            "session believes the token is still valid",
            Utc::now() + TimeDelta::hours(1),
        ),
    ] {
        let user = stack.user().await;
        let pair = TokenPair {
            token: mint_session_token(&user.pair.token, &stack.jwt_secret, expired_at),
            refresh_token: user.pair.refresh_token.clone(),
            expires_at: believed_expiry,
        };
        let session = Arc::new(HiveSession::new(&stack.registry, Some(pair)));
        let client = HiveRegistryClient::new(&stack.registry).with_session(Arc::clone(&session));

        let download = client.download(ECHO, SEEDED_VERSION).await;
        assert!(
            download.is_ok(),
            "F7 / PL-H6 (AGE-609), {case}: download with an expired access token: {:?}",
            download.err()
        );

        let queue = tempfile::tempdir().expect("a queue dir");
        let collector = UsageCollector::new(
            &stack.registry,
            UsageCollectorConfig {
                queue_dir: queue.path().to_path_buf(),
                ..Default::default()
            },
        );
        collector.set_session(Arc::clone(&session)).await;
        collector
            .record_invocation(ECHO, SEEDED_VERSION, Some(3), Some(2), None, Some(1))
            .await;
        let flushed = collector.flush().await;
        assert!(
            matches!(flushed, Ok(ref r) if r.accepted == 1),
            "F7 / PL-H6 (AGE-609), {case}: usage report after expiry: {flushed:?}"
        );
        assert!(
            std::fs::read_dir(queue.path())
                .expect("the queue dir")
                .next()
                .is_none(),
            "F7 / PL-H6 (AGE-609), {case}: the usage event is still queued after a successful flush"
        );
    }
}

// ── 5.4 ───────────────────────────────────────────────────────────────────

/// 5.4: a remote module chatty installed at 1.0.0 runs as 1.0.0 after 2.0.0
/// is published. v1 is echo ("Echo: …"), v2 is slow-host (the fake
/// LLM's reply), so the answer says which ran. Red today: the runner always
/// executes `latest_version` (F12) — PL-H8 (AGE-611) retires the runner's
/// module routes; the row then moves to the hosted-plugin path (PL-S6).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_04_remote_module_runs_the_installed_version() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let name = unique("pin-e2e");
    let remote = "execution_mode = \"remote\"\n";
    stack
        .publish_ok(
            &publisher,
            &flat_manifest(&name, "1.0.0", remote),
            stack.fixture(ECHO),
        )
        .await;

    let user = stack.user().await;
    let client = Arc::new(user.client(&stack.registry));
    let meta = client
        .get_module(&name)
        .await
        .expect("the module's metadata");
    install_from_hive(&client, &meta, "1.0.0")
        .await
        .expect("the remote install");

    stack
        .publish_ok(
            &publisher,
            &flat_manifest(&name, "2.0.0", remote),
            stack.fixture("slow-host"),
        )
        .await;

    let gateway = start_gateway(Arc::clone(&client), &stack.runner).await;
    let (status, content) = send_via_gateway(&gateway, &name, "ping").await;
    assert_eq!(
        (status, content.as_str()),
        (StatusCode::OK, "Echo: ping"),
        "F12 / PL-H8 (AGE-611): chatty installed {name}@1.0.0 (echo), 2.0.0 (slow-host) \
         was published after; the runner must run 1.0.0"
    );
}

// ── 5.5 ───────────────────────────────────────────────────────────────────

/// 5.5: `latest_version` is the highest semver, not the last published.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_05_latest_version_is_the_highest_semver() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let name = unique("semver-e2e");
    for version in ["2.0.0", "1.0.1"] {
        stack
            .publish_ok(
                &publisher,
                &flat_manifest(&name, version, ""),
                stack.fixture(ECHO),
            )
            .await;
    }
    let (_, module) = stack.module_json(&name).await;
    assert_eq!(
        module["latest_version"], "2.0.0",
        "published 2.0.0 then 1.0.1; latest_version moved backwards"
    );
}

// ── 5.6 ───────────────────────────────────────────────────────────────────

/// 5.6: a deleted version cannot be republished with different bytes
/// (versions are immutable, API spec §5).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_06_deleted_version_cannot_be_republished_with_other_bytes() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let name = unique("immut-e2e");
    for version in ["1.0.0", "1.1.0"] {
        stack
            .publish_ok(
                &publisher,
                &flat_manifest(&name, version, ""),
                stack.fixture(ECHO),
            )
            .await;
    }
    let (status, body) = stack.delete_version(&publisher, &name, "1.0.0").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "delete 1.0.0: {body}");

    let (status, body) = stack
        .publish(
            &publisher,
            &flat_manifest(&name, "1.0.0", ""),
            stack.fixture("stateful"),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "1.0.0 was deleted, then republished with different bytes and accepted: {body}"
    );
}

// ── 5.7 ───────────────────────────────────────────────────────────────────

/// 5.7: a paid module's billing round trip: acquire reserves, the module
/// runs, settle moves the balance by exactly the reported tokens, and
/// neither a repeated settle nor a repeated usage idempotency key charges
/// twice. (Whether a paid download needs a subscription is AGE-431's.)
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_07_paid_module_ledger_moves_by_the_reported_tokens_once() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let name = unique("paid-e2e");
    stack
        .publish_ok(
            &publisher,
            &flat_manifest(&name, "1.0.0", "pricing_model = \"paid\"\n"),
            stack.fixture(ECHO),
        )
        .await;
    stack.set_pricing(&publisher, &name, 0.01).await;

    let user = stack.user().await;
    let client = user.client(&stack.registry);
    let before = client
        .get_credit_balance()
        .await
        .expect("a balance")
        .balance_tokens;

    let session = client
        .acquire_session(&name, "1.0.0", 1000)
        .await
        .expect("acquire");
    assert_eq!(
        session.reserved_tokens, 1000,
        "acquire reserves the estimate"
    );

    // Invoke: the paid module, installed and run the way the desktop runs it.
    let meta = client
        .get_module(&name)
        .await
        .expect("the module's metadata");
    install_from_hive(&client, &meta, "1.0.0")
        .await
        .expect("the paid install");
    let mut modules = local_module_registry();
    let loaded = modules
        .load(module_dir().join(&name))
        .expect("the installed paid module loads");
    let module = modules.get(&loaded).expect("loaded");
    let reply = module
        .lock()
        .await
        .invoke_tool(chatty_wasm_runtime::ToolCallRequest {
            name: "echo".to_string(),
            arguments_json: r#"{"input":"bill me"}"#.to_string(),
            call_id: "paid".to_string(),
            caller: None,
        })
        .await
        .expect("the paid module runs");
    assert_eq!(reply.content, "bill me");

    let settled = client
        .settle_session(&session.session_id, 30, 20)
        .await
        .expect("settle");
    assert_eq!(
        (settled.tokens_deducted, settled.balance_after),
        (50, before - 50),
        "settle deducts the 30 + 20 reported tokens and releases the rest of the reservation"
    );
    let again = client
        .settle_session(&session.session_id, 30, 20)
        .await
        .expect("settle again");
    let balance = client
        .get_credit_balance()
        .await
        .expect("a balance")
        .balance_tokens;
    assert_eq!(
        (again.balance_after, balance),
        (before - 50, before - 50),
        "a repeated settle of the same session charges nothing more"
    );

    let event = usage_event(&name, "invocation");
    let first = client
        .report_usage(vec![event.clone()])
        .await
        .expect("report");
    let replay = client
        .report_usage(vec![event])
        .await
        .expect("report again");
    assert_eq!(
        (
            first.accepted,
            first.duplicates,
            replay.accepted,
            replay.duplicates
        ),
        (1, 0, 0, 1),
        "a repeated usage idempotency key is a duplicate, not a second charge"
    );
}

// ── 5.8 ───────────────────────────────────────────────────────────────────

/// A usage collector against the stack as `user`, queueing into `queue`.
async fn collector(
    stack: &Stack,
    user: &hive_e2e::User,
    queue: &std::path::Path,
) -> UsageCollector {
    let collector = UsageCollector::new(
        &stack.registry,
        UsageCollectorConfig {
            queue_dir: queue.to_path_buf(),
            max_buffer_size: 1000,
            ..Default::default()
        },
    );
    collector.set_session(user.session(&stack.registry)).await;
    collector
}

fn queued(queue: &std::path::Path) -> usize {
    std::fs::read_dir(queue)
        .expect("the queue dir")
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
        .filter_map(|text| serde_json::from_str::<Vec<Value>>(&text).ok())
        .map(|events| events.len())
        .sum()
}

/// 5.8: a batch the registry rejects as invalid (a 4xx; hive answers 422)
/// does not poison the queue: the next flush sends the next event. Red
/// today: the rejected batch is re-queued and re-sent forever (F13) —
/// PL-H9 (AGE-612).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_08_usage_queue_drains_after_a_rejected_batch() {
    let stack = Stack::from_env();
    let user = stack.user().await;
    let queue = tempfile::tempdir().expect("a queue dir");
    let collector = collector(&stack, &user, queue.path()).await;

    collector
        .record(usage_event(ECHO, "not-an-event-type"))
        .await;
    let rejected = collector.flush().await;
    assert!(
        rejected.is_err(),
        "the registry accepts a bogus event_type: {rejected:?}"
    );

    collector.record(usage_event(ECHO, "invocation")).await;
    let next = collector.flush().await;
    assert!(
        matches!(next, Ok(ref r) if r.accepted == 1) && queued(queue.path()) == 0,
        "F13 / PL-H9 (AGE-612): after one rejected batch the queue is poisoned: next flush {next:?}, \
         {} event(s) still queued",
        queued(queue.path())
    );
}

/// 5.8: more than the registry's 100-event batch limit goes out in batches
/// and the queue drains. Red today: all 150 go in one request, the registry
/// rejects it (422), and the whole lot is re-queued (F13) — PL-H9 (AGE-612).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_08_usage_queue_sends_more_than_100_events_in_batches() {
    let stack = Stack::from_env();
    let user = stack.user().await;
    let queue = tempfile::tempdir().expect("a queue dir");
    let collector = collector(&stack, &user, queue.path()).await;

    for _ in 0..150 {
        collector.record(usage_event(ECHO, "invocation")).await;
    }
    let mut accepted = 0;
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        let flushed = collector.flush().await;
        if let Ok(r) = &flushed {
            accepted += r.accepted;
        }
        outcomes.push(format!("{flushed:?}"));
    }
    assert!(
        accepted == 150 && queued(queue.path()) == 0,
        "F13 / PL-H9 (AGE-612): 150 events, {accepted} accepted, {} still queued; flushes: {}",
        queued(queue.path()),
        outcomes.join(" | ")
    );
}

// ── 5.9 ───────────────────────────────────────────────────────────────────

/// 5.9: the registry refuses a core (non-component) wasm; chatty and the
/// runner only load components.
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_09_registry_rejects_a_core_module_upload() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let name = unique("core-e2e");
    let (status, body) = stack
        .publish(
            &publisher,
            &flat_manifest(&name, "1.0.0", ""),
            stack.fixture("core-module"),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a core wasm module (not a component) was accepted: {body}"
    );
}

// ── 5.10 ──────────────────────────────────────────────────────────────────

/// 5.10: a remote module's chat goes chatty gateway → hive-runner → the
/// runner's LLM upstream (the stack's wiremock) and back. PL-H8 (AGE-611)
/// retires this path; the row then moves to the hosted-plugin path (PL-S6).
#[tokio::test]
#[ignore = "needs hive stack; run by nightly"]
async fn s5_10_remote_module_chat_reaches_the_runner_llm_upstream() {
    let stack = Stack::from_env();
    let publisher = stack.publisher().await;
    let name = unique("remote-e2e");
    stack
        .publish_ok(
            &publisher,
            &flat_manifest(&name, "1.0.0", "execution_mode = \"remote\"\n"),
            stack.fixture("slow-host"),
        )
        .await;

    let user = stack.user().await;
    let client = Arc::new(user.client(&stack.registry));
    let meta = client
        .get_module(&name)
        .await
        .expect("the module's metadata");
    install_from_hive(&client, &meta, "1.0.0")
        .await
        .expect("the remote install");

    let gateway = start_gateway(Arc::clone(&client), &stack.runner).await;
    let (status, content) = send_via_gateway(&gateway, &name, "hello").await;
    assert_eq!(
        (status, content.as_str()),
        (
            StatusCode::OK,
            "Hello from the fake LLM upstream (hive e2e stack)."
        ),
        "gateway → runner → LLM upstream round trip for {name}"
    );
}
