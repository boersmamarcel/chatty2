//! chatty-server's wire, recorded and replayed against the client (AGE-2083,
//! HX-4).
//!
//! The fixtures under `crates/hive-client/tests/recorded/chatty_server/` are
//! what the server's `/api` routes answer, as files: two JSON bodies and the
//! SSE streams of a turn. The `SessionEvent` and `ApprovalRequest` payloads in
//! them are serialized from the real Rust values by [`record_fixtures`]
//! (`UPDATE_GOLDENS=1`), so they cannot drift from the types; every other test
//! here only reads the files and replays them through [`HostedSession`].
//!
//! Once hive's nightly job records these (`HIVE_E2E_RECORD=1`), only that job
//! may rewrite them (see the directory's README); until then the generator is
//! the source.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rig_core::completion::{AssistantContent, Message};
use rig_core::message::{AssistantMessage, CallId, ToolResult, ToolResultContent, UserContent};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::TurnInput;
use super::event::SessionEvent;
use super::handoff::{HandoffServer, fetch_conversation};
use super::hosted::{BrokerRequest, HostedFrame, HostedSession, InputOutcome};
use crate::models::token_usage::{ApiCallUsage, ModelRef};
use crate::settings::models::providers_store::ProviderType;

const STARTED_AT_MS: u64 = 1_795_627_398_000;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hive-client/tests/recorded/chatty_server")
}

fn read(name: &str) -> String {
    let file = dir().join(name);
    std::fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("{}: {e}; re-run with UPDATE_GOLDENS=1", file.display()))
}

/// The `event:` name the server gives a session event: its variant, snake_case.
fn frame_name(event: &SessionEvent) -> &'static str {
    match event {
        SessionEvent::TurnStarted => "turn_started",
        SessionEvent::Text(_) => "text",
        SessionEvent::ToolCallStarted { .. } => "tool_call_started",
        SessionEvent::ToolCallInput { .. } => "tool_call_input",
        SessionEvent::ToolCallResult { .. } => "tool_call_result",
        SessionEvent::ApiCallUsage(_) => "api_call_usage",
        SessionEvent::TurnMessages(_) => "turn_messages",
        SessionEvent::TurnEnded => "turn_ended",
        other => panic!("no recorded frame name for {other:?}"),
    }
}

fn sse(name: &str, data: &Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn session_frames(events: &[SessionEvent]) -> String {
    events
        .iter()
        .map(|e| sse(frame_name(e), &serde_json::to_value(e).unwrap()))
        .collect()
}

fn tool_call_messages() -> Vec<Message> {
    let call = |id: &str, name: &str, args: Value| {
        Message::Assistant(AssistantMessage::new(vec![AssistantContent::tool_call(
            id,
            crate::testing::tool_name(name),
            args,
        )]))
    };
    let result = |id: &str, name: &str, text: &str| Message::User {
        content: vec![UserContent::ToolResult(ToolResult {
            call: CallId::from_wire(id),
            name: crate::testing::tool_name(name),
            content: vec![ToolResultContent::text(text)],
            is_error: false,
        })],
    };
    vec![
        Message::user("What does the README say?"),
        call("call_1", "read_file", json!({"path": "README.md"})),
        result("call_1", "read_file", "# chatty"),
        Message::assistant("It starts with the title: chatty."),
    ]
}

fn turn_events() -> Vec<SessionEvent> {
    let usage = ApiCallUsage {
        turn: 1,
        input_tokens: 120,
        output_tokens: 18,
        model: Some(ModelRef {
            provider: ProviderType::OpenRouter,
            model_id: "anthropic/claude-sonnet".into(),
        }),
        duration_ms: 850,
        ..Default::default()
    };
    vec![
        SessionEvent::TurnStarted,
        SessionEvent::Text("Let me look. ".into()),
        SessionEvent::ToolCallStarted {
            id: "call_1".into(),
            name: "read_file".into(),
        },
        SessionEvent::ToolCallInput {
            id: "call_1".into(),
            arguments: r#"{"path":"README.md"}"#.into(),
        },
        SessionEvent::ToolCallResult {
            id: "call_1".into(),
            result: "# chatty".into(),
        },
        SessionEvent::Text("It starts with the title: chatty.".into()),
        SessionEvent::ApiCallUsage(usage),
        SessionEvent::TurnMessages(tool_call_messages()),
        SessionEvent::TurnEnded,
    ]
}

fn approval_request() -> chatty_fabric::ApprovalRequest {
    chatty_fabric::ApprovalRequest {
        kind: chatty_fabric::ApprovalKind::Exec,
        command_or_path: "cargo test -p chatty-core".into(),
        diff_stat: None,
        asker: None,
    }
}

/// Write every fixture. Gated on `UPDATE_GOLDENS=1`; anything else is a no-op
/// so a plain `cargo test` never touches the recordings.
#[test]
fn record_fixtures() {
    if std::env::var("UPDATE_GOLDENS").is_err() {
        return;
    }
    let dir = dir();
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, text: String| std::fs::write(dir.join(name), text).unwrap();
    let json_file = |value: Value| serde_json::to_string_pretty(&value).unwrap() + "\n";

    write(
        "conversations_page.json",
        json_file(json!({
            "conversations": [
                {"id": "c-2", "title": "Second chat", "model_id": "anthropic/claude-sonnet", "turn_active": true},
                {"id": "c-1", "title": "Read the README", "model_id": "anthropic/claude-sonnet", "turn_active": false}
            ],
            "next_cursor": "opaque-cursor-1"
        })),
    );
    write(
        "conversation_detail.json",
        json_file(json!({
            "id": "c-1",
            "title": "Read the README",
            "model_id": "anthropic/claude-sonnet",
            "turn_active": false,
            "messages": tool_call_messages(),
        })),
    );
    write("turn.sse", session_frames(&turn_events()));

    let attached = sse(
        "turn_attached",
        &json!({"started_at_ms": STARTED_AT_MS, "dropped": 0}),
    );
    let tail = [
        SessionEvent::Text("It starts with the title: chatty.".into()),
        SessionEvent::TurnEnded,
    ];
    write(
        "turn_events_reattach.sse",
        attached + &session_frames(&tail),
    );

    let approval = sse(
        "broker_approval",
        &json!({"id": "approval-1", "nonce": "nonce-7f3a", "request": approval_request()}),
    );
    let settled = sse(
        "broker_input_settled",
        &json!({"id": "approval-1", "outcome": "approved"}),
    );
    write(
        "broker_approval.sse",
        session_frames(&[SessionEvent::TurnStarted])
            + &approval
            + &settled
            + &session_frames(&[SessionEvent::TurnEnded]),
    );
    write(
        "connector_needs_sign_in.sse",
        session_frames(&[SessionEvent::TurnStarted])
            + &sse("connector_needs_sign_in", &json!({"connector": "notion"}))
            + &session_frames(&[SessionEvent::TurnEnded]),
    );
    write(
        "stream_error.sse",
        session_frames(&[SessionEvent::TurnStarted])
            + &sse(
                "stream_error",
                &json!({"message": "the workspace could not be pushed", "code": "workspace_push_failed", "commit": "abc123"}),
            )
            + &session_frames(&[SessionEvent::TurnEnded]),
    );
}

/// What one replayed turn produced.
struct Replay {
    events: Vec<SessionEvent>,
    frames: Vec<HostedFrame>,
}

/// Serve `body` as the answer to a turn's POST and run one turn against it.
async fn replay(body: String) -> Replay {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/conversations/c-1/turns"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(body.into_bytes(), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let mut session = HostedSession::new(server.uri(), "c-1");
    let frames = Arc::new(Mutex::new(Vec::new()));
    let sink = frames.clone();
    session.set_frame_sink(Arc::new(move |frame| sink.lock().unwrap().push(frame)));
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    session
        .begin_turn(&TurnInput::text("hello"), move |event| {
            seen.lock().unwrap().push(event)
        })
        .expect("the turn starts")
        .await;
    let events = events.lock().unwrap().clone();
    let frames = frames.lock().unwrap().clone();
    Replay { events, frames }
}

fn count(events: &[SessionEvent], is: impl Fn(&SessionEvent) -> bool) -> usize {
    events.iter().filter(|e| is(e)).count()
}

/// The contract every hosted turn keeps whatever the server sent: it starts
/// with `TurnStarted`, ends with `TurnEnded`, and has exactly one of each.
fn assert_well_formed(events: &[SessionEvent]) {
    assert!(
        matches!(events.first(), Some(SessionEvent::TurnStarted)),
        "{events:?}"
    );
    assert!(
        matches!(events.last(), Some(SessionEvent::TurnEnded)),
        "{events:?}"
    );
    assert_eq!(
        count(events, |e| matches!(e, SessionEvent::TurnStarted)),
        1,
        "{events:?}"
    );
    assert_eq!(
        count(events, |e| matches!(e, SessionEvent::TurnEnded)),
        1,
        "{events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, SessionEvent::Error(_))),
        "{events:?}"
    );
}

#[test]
fn the_conversation_page_has_summaries_and_an_opaque_cursor() {
    let page: Value = serde_json::from_str(&read("conversations_page.json")).unwrap();
    let list = page["conversations"]
        .as_array()
        .expect("a conversations array");
    assert!(!list.is_empty());
    for summary in list {
        for key in ["id", "title", "model_id", "turn_active"] {
            assert!(
                summary.get(key).is_some(),
                "a summary lacks {key}: {summary}"
            );
        }
    }
    assert!(page["next_cursor"].is_string(), "{page}");
}

#[tokio::test]
async fn the_conversation_detail_decodes_with_its_tool_call_messages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/conversations/c-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(read("conversation_detail.json"), "application/json"),
        )
        .mount(&server)
        .await;
    let detail = fetch_conversation(&HandoffServer::new(&server.uri(), None), "c-1")
        .await
        .unwrap();
    assert_eq!(detail.id, "c-1");
    assert_eq!(detail.model_id, "anthropic/claude-sonnet");
    assert!(!detail.turn_active);
    assert_eq!(detail.messages.len(), 4);
    assert!(matches!(
        &detail.messages[1],
        Message::Assistant(a) if a.content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)))
    ));
    assert!(matches!(
        &detail.messages[2],
        Message::User { content } if content.iter().any(|c| matches!(c, UserContent::ToolResult(_)))
    ));
}

#[tokio::test]
async fn a_recorded_turn_replays_as_the_session_events_it_was() {
    let replay = replay(read("turn.sse")).await;
    assert_well_formed(&replay.events);
    assert!(replay.frames.is_empty(), "{:?}", replay.frames);
    let names: Vec<_> = replay
        .events
        .iter()
        .map(|e| match e {
            SessionEvent::Text(_) => "text",
            SessionEvent::ToolCallStarted { .. } => "tool_started",
            SessionEvent::ToolCallInput { .. } => "tool_input",
            SessionEvent::ToolCallResult { .. } => "tool_result",
            SessionEvent::ApiCallUsage(_) => "usage",
            SessionEvent::TurnMessages(m) => {
                assert_eq!(m.len(), 4);
                "messages"
            }
            SessionEvent::TurnStarted => "start",
            SessionEvent::TurnEnded => "end",
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        names,
        [
            "start",
            "text",
            "tool_started",
            "tool_input",
            "tool_result",
            "text",
            "usage",
            "messages",
            "end"
        ]
    );
}

#[tokio::test]
async fn a_recorded_reattach_stream_opens_with_its_marker_then_session_events() {
    let replay = replay(read("turn_events_reattach.sse")).await;
    assert_well_formed(&replay.events);
    assert!(
        matches!(
            replay.frames.as_slice(),
            [HostedFrame::Attached {
                started_at_ms: STARTED_AT_MS,
                dropped: 0
            }]
        ),
        "{:?}",
        replay.frames
    );
    assert!(
        replay
            .events
            .iter()
            .any(|e| matches!(e, SessionEvent::Text(_)))
    );
}

#[tokio::test]
async fn a_recorded_approval_and_its_settlement_reach_the_sink_in_order() {
    let replay = replay(read("broker_approval.sse")).await;
    assert_well_formed(&replay.events);
    match replay.frames.as_slice() {
        [
            HostedFrame::BrokerInput {
                id,
                nonce,
                request: BrokerRequest::Approval(request),
            },
            HostedFrame::BrokerInputSettled {
                id: settled,
                outcome: InputOutcome::Approved,
            },
        ] => {
            assert_eq!((id.as_str(), nonce.as_str()), ("approval-1", "nonce-7f3a"));
            assert_eq!(settled, id);
            assert_eq!(request, &approval_request());
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_recorded_connector_sign_in_is_a_frame_and_the_turn_still_ends() {
    let replay = replay(read("connector_needs_sign_in.sse")).await;
    assert_well_formed(&replay.events);
    assert!(
        matches!(replay.frames.as_slice(), [HostedFrame::ConnectorNeedsSignIn { connector }] if connector == "notion"),
        "{:?}",
        replay.frames
    );
}

/// A `stream_error` frame is the server saying it could not serialize an
/// event: the client reports it as an `Error` rather than dropping a hole.
#[tokio::test]
async fn a_recorded_stream_error_is_reported_and_the_turn_still_ends() {
    let replay = replay(read("stream_error.sse")).await;
    assert!(matches!(
        replay.events.first(),
        Some(SessionEvent::TurnStarted)
    ));
    assert!(matches!(
        replay.events.last(),
        Some(SessionEvent::TurnEnded)
    ));
    assert_eq!(
        count(&replay.events, |e| matches!(e, SessionEvent::TurnEnded)),
        1
    );
    assert!(
        replay.events.iter().any(|e| matches!(
            e,
            SessionEvent::Error(error) if error.message == "the workspace could not be pushed"
        )),
        "{:?}",
        replay.events
    );
}

/// A frame from a newer server is skipped wherever it lands in a recording.
#[tokio::test]
async fn an_unknown_frame_spliced_into_a_recording_is_skipped() {
    let recorded = read("turn.sse");
    let (head, tail) = recorded.split_once("event: text").unwrap();
    let body = format!("{head}event: from_the_future\ndata: {{\"x\":1}}\n\nevent: text{tail}");
    let replay = replay(body).await;
    assert_well_formed(&replay.events);
    assert!(replay.frames.is_empty(), "{:?}", replay.frames);
    assert_eq!(replay.events.len(), turn_events().len());
}
