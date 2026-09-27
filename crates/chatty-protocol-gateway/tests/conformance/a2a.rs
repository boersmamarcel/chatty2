//! S3 rows 3.7–3.9: A2A, through chatty-core's own `A2aClient` (the client
//! `invoke_agent` uses) wherever it can say what the row needs.

use std::time::{Duration, Instant};

use chatty_core::services::a2a_client::{A2aClient, A2aStreamEvent};
use chatty_wasm_runtime::test_support::FakeResponse;
use chatty_wasm_runtime::{Role, ToolCall};
use futures::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::harness::{Gateway, Module};

/// Every user-turn text the fake provider saw on call `index`.
fn user_texts(gw: &Gateway, index: usize) -> Vec<String> {
    gw.llm.calls()[index]
        .messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .collect()
}

fn message_send(parts: Value, context_id: &str, message_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/send",
        "params": {
            "message": {
                "role": "user",
                "messageId": message_id,
                "contextId": context_id,
                "parts": parts,
            }
        }
    })
}

/// 3.7 — `message/send` with a multi-part message: every text part reaches
/// the guest, not only `parts[0]` (F11). echo-agent forwards its request to
/// the fake provider ("use llm"), so the provider sees what the guest saw.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H4 (AGE-607)"]
async fn s3_07_a2a_message_send_all_parts_reach_guest() {
    let gw = Gateway::start(
        vec![Module::shipped("echo-agent")],
        vec![FakeResponse::Text("ok".into())],
    )
    .await;

    let (status, body) = gw
        .post(
            "/a2a/echo-agent",
            &message_send(
                json!([
                    { "kind": "text", "text": "use llm: part one" },
                    { "kind": "text", "text": "part two" },
                ]),
                "ctx-parts",
                "m1",
            ),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"]["status"]["state"], "completed", "{body}");

    let seen = user_texts(&gw, 0).join("\n");
    for part in ["use llm: part one", "part two"] {
        assert!(
            seen.contains(part),
            "`{part}` did not reach the guest: {seen:?}"
        );
    }
}

/// 3.7 — history: a second `message/send` in the same `contextId` reaches the
/// guest with the first turn in front of it. Today every message is a fresh
/// one-message conversation.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "known defect: PL-H4 (AGE-607)"]
async fn s3_07_a2a_message_send_history_reaches_guest() {
    let gw = Gateway::start(
        vec![Module::shipped("echo-agent")],
        vec![
            FakeResponse::Text("first answer".into()),
            FakeResponse::Text("second answer".into()),
        ],
    )
    .await;

    for (id, text) in [("m1", "use llm: turn one"), ("m2", "use llm: turn two")] {
        let (status, body) = gw
            .post(
                "/a2a/echo-agent",
                &message_send(json!([{ "kind": "text", "text": text }]), "ctx-history", id),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    let seen = user_texts(&gw, 1);
    assert!(
        seen.iter().any(|t| t == "use llm: turn one"),
        "the second turn's guest call carries the first: {seen:?}"
    );
}

/// Collect a `message/stream` through `A2aClient` until its final event.
async fn stream_events(gw: &Gateway, module: &str, prompt: &str) -> Vec<A2aStreamEvent> {
    let client = A2aClient::new();
    let mut stream = client
        .send_message_stream(&gw.a2a_agent(module), prompt)
        .await
        .unwrap_or_else(|e| panic!("{module}: message/stream failed: {e:#}"));
    let mut events = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(30), stream.next())
        .await
        .expect("an event within thirty seconds")
    {
        events.push(event.expect("a well-formed event"));
    }
    events
}

/// The events as a compact sequence: `working`, `progress:<text>`,
/// `artifact:<text>`, `completed`, `failed:<reason>`.
fn shape(events: &[A2aStreamEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match event {
            A2aStreamEvent::StatusUpdate {
                state,
                message: None,
                ..
            } => state.clone(),
            A2aStreamEvent::StatusUpdate {
                state,
                message: Some(message),
                ..
            } if state == "working" => format!("progress:{message}"),
            A2aStreamEvent::StatusUpdate {
                state,
                message: Some(message),
                ..
            } => format!("{state}:{message}"),
            A2aStreamEvent::ArtifactUpdate { text, .. } => format!("artifact:{text}"),
        })
        .collect()
}

/// 3.8 — `message/stream` goes working → progress → artifact → completed,
/// with `log-flood`'s log lines as the progress; and with benford's full
/// tool loop behind a scripted model; and a failing guest ends `failed`.
#[tokio::test(flavor = "multi_thread")]
async fn s3_08_a2a_message_stream_lifecycle() {
    // log-flood: three log lines become three progress events, in order.
    let gw = Gateway::start(vec![Module::fixture("log-flood")], vec![]).await;
    assert_eq!(
        shape(&stream_events(&gw, "log-flood", "3").await),
        vec![
            "working",
            "progress:log-flood line 0",
            "progress:log-flood line 1",
            "progress:log-flood line 2",
            "artifact:logged 3 lines",
            "completed",
        ]
    );

    // benford: the model calls both tools, then writes the report.
    let call = |id: &str, name: &str, arguments: Value| ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.to_string(),
    };
    let gw = Gateway::start(
        vec![Module::shipped("benford-agent")],
        vec![
            FakeResponse::ToolCalls(vec![call(
                "c1",
                "compute_benford_distribution",
                json!({ "numbers": [123.4, 187.0, 1450.0, 212.5, 2999.0, 31.0] }),
            )]),
            FakeResponse::ToolCalls(vec![call(
                "c2",
                "chi_square_test",
                json!({ "observed_counts": [3, 2, 1, 0, 0, 0, 0, 0, 0], "total": 6 }),
            )]),
            FakeResponse::Text("AUDIT REPORT: low risk".into()),
        ],
    )
    .await;
    let events = shape(&stream_events(&gw, "benford-agent", "audit these").await);
    assert_eq!(events.first().map(String::as_str), Some("working"));
    assert_eq!(
        events[events.len() - 2..],
        ["artifact:AUDIT REPORT: low risk", "completed"],
        "{events:#?}"
    );
    for tool in ["compute_benford_distribution", "chi_square_test"] {
        assert!(
            events
                .iter()
                .any(|e| e.starts_with("progress:") && e.contains(tool)),
            "benford's `{tool}` call shows as progress: {events:#?}"
        );
    }
    let calls = gw.llm.calls();
    assert_eq!(calls.len(), 3, "the guest ran its loop against the model");
    assert!(
        calls[2]
            .messages
            .iter()
            .any(|m| m.content.contains("chi_square")),
        "the tool results went back to the model"
    );

    // Failure path: the model errors, the guest returns the error, the task
    // ends `failed` with the reason.
    let gw = Gateway::start(
        vec![Module::shipped("benford-agent")],
        vec![FakeResponse::Err("upstream is down".into())],
    )
    .await;
    let events = shape(&stream_events(&gw, "benford-agent", "audit these").await);
    assert_eq!(events.first().map(String::as_str), Some("working"));
    let last = events.last().unwrap();
    assert!(
        last.starts_with("failed:") && last.contains("upstream is down"),
        "{events:#?}"
    );
    assert!(
        !events.iter().any(|e| e.starts_with("artifact:")),
        "{events:#?}"
    );
}

/// 3.9 — the client disconnects mid-stream.
///
/// Decision (the plan says "cancelled or bounded"): **bounded, not
/// cancelled**, pinned as today's behaviour. The guest call is a synchronous
/// WASM call on a blocking thread; a client disconnect does not stop it. So
/// the gateway lets it run to the end, then releases the module: the dropped
/// call still reaches the model, and the next caller is served as soon as it
/// returns — no thread or lock is left behind. How long "the end" may be is
/// bounded by the runtime's per-call wall clock, host time included (PL-H1's
/// epoch deadline, S1 rows 1.3/1.4).
#[tokio::test(flavor = "multi_thread")]
async fn s3_09_a2a_disconnect_mid_stream_is_bounded() {
    const HOST_DELAY: Duration = Duration::from_secs(2);
    let gw = Gateway::start(
        vec![Module::fixture("slow-host")],
        vec![
            FakeResponse::Delay(HOST_DELAY, "abandoned".into()),
            FakeResponse::Text("after".into()),
        ],
    )
    .await;

    let started = Instant::now();
    let client = A2aClient::new();
    let mut stream = client
        .send_message_stream(&gw.a2a_agent("slow-host"), "first")
        .await
        .unwrap();
    let first = stream.next().await.expect("an event").unwrap();
    assert_eq!(shape(&[first]), ["working"]);
    drop(stream);
    drop(client);

    let (status, body) = gw
        .post(
            "/v1/slow-host/chat/completions",
            &json!({ "model": "slow-host", "messages": [{ "role": "user", "content": "second" }] }),
        )
        .await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "after");
    assert_eq!(
        gw.llm.calls().len(),
        2,
        "the abandoned call ran to its end (not cancelled)"
    );
    assert!(
        elapsed < HOST_DELAY + Duration::from_secs(3),
        "the module was released once the abandoned call returned; the next \
         call finished {elapsed:?} after the first started (host delay {HOST_DELAY:?})"
    );
}
