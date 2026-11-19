//! A stalled turn, or one a connection failure ended, stays in the history
//! the next request is built from (AGE-1438); other errors roll back.
//!
//! hive's queue continues a stalled issue with a "continue" message. The
//! stall watchdog drops the stream, so rig hands back no record of the
//! turn, and its tool calls and results used to be lost — after 21 to 129
//! calls of work — and a stall before any output rolled back the task's own
//! message. These tests stall a turn, send the next one to the fake model
//! and check what that request carries, and that its shape is one every
//! provider accepts.

use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::agent_spec::AgentSpec;
use crate::services::llm_service::StreamChunk;
use crate::services::{
    Scenario, ScriptedItem, StreamSurface, kept_failed_turn_note, stalled_stream_message,
    with_stall_scale,
};
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
use crate::testing::fake_model::{FakeDaemon, RecordedRequest, Reply, Script};

const MODEL: &str = "age-1438-model";

/// Watchdog durations divided by this: the plain stall timeout is 3 s.
const STALL_SCALE: u32 = 60;

/// A session on the fake model, with the `calculate` tool on offer.
async fn session_on(daemon: &FakeDaemon) -> (AgentSession, tempfile::TempDir) {
    let _ = crate::init_repositories();
    let workspace = tempfile::tempdir().expect("a workspace");
    let settings = ExecutionSettingsModel {
        workspace_dir: Some(workspace.path().to_string_lossy().into_owned()),
        fetch_enabled: false,
        ..ExecutionSettingsModel::default()
    };
    let model = ModelConfig::new(
        MODEL.to_string(),
        MODEL.to_string(),
        ProviderType::OpenRouter,
        MODEL.to_string(),
    );
    let provider = ProviderConfig::new("Fake".to_string(), ProviderType::OpenRouter)
        .with_api_key("test-key".to_string())
        .with_base_url(daemon.base_url());
    let built = AgentBuildContext::from_spec(
        &AgentSpec::named("data-analyst"),
        crate::factories::agent_factory::AgentServices {
            exec_settings: Some(settings.clone()),
            ..Default::default()
        },
    )
    .expect("the spec builds");
    let mut session = AgentSession::new(AgentSessionConfig {
        execution_settings: settings,
        surface: StreamSurface::Headless,
        loop_guard: false,
    });
    session
        .create_conversation(
            "age-1438".to_string(),
            "Stalled task".to_string(),
            &model,
            &provider,
            built.context,
        )
        .await
        .expect("the conversation builds");
    (session, workspace)
}

/// Drive a turn to its end, apply its events as an owner does and finish
/// it: the events, and how `finish_turn` committed it.
async fn finish(
    session: &mut AgentSession,
    events: Rc<RefCell<Vec<SessionEvent>>>,
    turn: impl Future<Output = ()>,
) -> (Vec<SessionEvent>, TurnOutcome) {
    with_stall_scale(STALL_SCALE, turn).await;
    let events = Rc::try_unwrap(events).unwrap().into_inner();
    for event in &events {
        session.apply(event);
    }
    let outcome = session.finish_turn(None, vec![]).expect("a turn to finish");
    (events, outcome)
}

async fn run_turn(session: &mut AgentSession, text: &str) -> (Vec<SessionEvent>, TurnOutcome) {
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_turn(TurnInput::text(text), move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the turn starts");
    finish(session, events, turn).await
}

async fn run_scripted_turn(
    session: &mut AgentSession,
    text: &str,
    items: Vec<ScriptedItem>,
) -> (Vec<SessionEvent>, TurnOutcome) {
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let scenario = Scenario {
        name: "stalled",
        progress: Vec::new(),
        items,
    };
    let turn = session
        .begin_scripted_turn(TurnInput::text(text), scenario, move |event| {
            sink.borrow_mut().push(event)
        })
        .expect("the turn starts");
    finish(session, events, turn).await
}

fn stalled() -> ScriptedItem {
    ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
        StreamErrorKind::Stalled,
        stalled_stream_message(crate::services::STALL_TIMEOUT),
    )))
}

fn stall_error(events: &[SessionEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, SessionEvent::Error(e) if e.kind == StreamErrorKind::Stalled))
}

/// A request's `messages`, without the system prompt.
fn messages(request: &RecordedRequest) -> Vec<serde_json::Value> {
    request.json()["messages"]
        .as_array()
        .expect("the request carries messages")
        .iter()
        .filter(|m| m["role"] != "system")
        .cloned()
        .collect()
}

/// The text of a message's `content`, whether a string or a list of parts.
fn content_text(message: &serde_json::Value) -> String {
    match &message["content"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// The tool-call ids an assistant message makes.
fn call_ids(message: &serde_json::Value) -> Vec<String> {
    message["tool_calls"]
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .filter_map(|call| call["id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The order every provider accepts, checked on an OpenAI-shaped request:
/// every tool call is answered by the tool messages straight after it and
/// every tool message answers such a call (OpenAI, Ollama); an assistant
/// message says something or calls something (Anthropic rejects an empty
/// text block); and a user message follows a reply, never another user
/// message or a tool result (Gemini and Anthropic alternate turns).
fn assert_valid_order(messages: &[serde_json::Value]) {
    let dump = || serde_json::to_string_pretty(messages).unwrap();
    let mut pending: Vec<String> = Vec::new();
    let mut previous: Option<&str> = None;
    for message in messages {
        let role = message["role"].as_str().expect("a role");
        if role == "tool" {
            let id = message["tool_call_id"].as_str().expect("a tool_call_id");
            let at = pending.iter().position(|p| p == id);
            assert!(
                at.is_some(),
                "a tool result answers no open call {id}: {}",
                dump()
            );
            pending.remove(at.unwrap());
        } else {
            assert!(
                pending.is_empty(),
                "calls left unanswered {pending:?}: {}",
                dump()
            );
        }
        match role {
            "assistant" => {
                let calls = call_ids(message);
                assert!(
                    !calls.is_empty() || !content_text(message).trim().is_empty(),
                    "an empty assistant message: {}",
                    dump()
                );
                pending = calls;
            }
            "user" => assert!(
                matches!(previous, None | Some("assistant")),
                "a user message after {previous:?}: {}",
                dump()
            ),
            _ => {}
        }
        previous = Some(role);
    }
    assert!(
        pending.is_empty(),
        "calls left unanswered {pending:?}: {}",
        dump()
    );
}

/// A turn that stalls after two tool calls, on the provider's side: the
/// "continue" request carries both calls and their results, in order, and
/// the task they were made for.
#[tokio::test]
async fn a_turn_stalled_after_two_tool_calls_keeps_both_for_the_next_request() {
    let daemon = FakeDaemon::scripted(Script::new().route(
        MODEL,
        [
            Reply::tool_call("calculate", serde_json::json!({"expression": "1 + 1"})),
            Reply::tool_call("calculate", serde_json::json!({"expression": "20 + 22"})),
            // The third call goes quiet past the (scaled) stall timeout.
            Reply::Delay(10_000),
            Reply::text("too late"),
            Reply::text("Resumed: the answer is 42."),
        ],
    ));
    let (mut session, _workspace) = session_on(&daemon).await;

    let (events, outcome) = run_turn(&mut session, "Add up the numbers.").await;
    assert!(stall_error(&events), "the turn stalled: {events:#?}");
    assert_eq!(outcome, TurnOutcome::Persisted);

    let (_, outcome) = run_turn(&mut session, "continue").await;
    assert_eq!(outcome, TurnOutcome::Persisted);

    let requests = daemon.requests();
    assert_eq!(
        requests.len(),
        4,
        "three calls in the stalled turn, one after"
    );
    let sent = messages(&requests[3]);
    assert_valid_order(&sent);
    let dump = serde_json::to_string_pretty(&sent).unwrap();
    assert!(
        content_text(&sent[0]).contains("Add up the numbers."),
        "{dump}"
    );
    let calls: Vec<String> = sent.iter().flat_map(call_ids).collect();
    assert_eq!(calls.len(), 2, "both calls: {dump}");
    let results: Vec<String> = sent
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(content_text)
        .collect();
    assert_eq!(results.len(), 2, "both results: {dump}");
    assert!(results[0].contains('2'), "{dump}");
    assert!(results[1].contains("42"), "{dump}");
    assert_eq!(content_text(sent.last().unwrap()), "continue", "{dump}");

    // What is persisted is what was sent: a restored conversation carries
    // the same round-trips.
    let history = session.conversation().unwrap().messages();
    let restored = Conversation::deserialize_history(&serde_json::to_string(&history).unwrap())
        .expect("the history round-trips");
    assert_eq!(restored, history);
}

/// A turn that stalls with one tool call still running: the call that got
/// its result stays, the one that did not is left out with its call, and
/// the next request is valid.
#[tokio::test]
async fn a_turn_stalled_with_a_tool_still_running_sends_a_valid_request() {
    let daemon = FakeDaemon::scripted(Script::new().route(MODEL, [Reply::text("Resumed.")]));
    let (mut session, _workspace) = session_on(&daemon).await;
    let call = |id: &str, expression: &str| {
        [
            ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                id: id.into(),
                name: "calculate".into(),
            }),
            ScriptedItem::Chunk(StreamChunk::ToolCallInput {
                id: id.into(),
                arguments: serde_json::json!({ "expression": expression }).to_string(),
            }),
        ]
    };
    let mut items = Vec::new();
    items.extend(call("call_done", "1 + 1"));
    items.push(ScriptedItem::Chunk(StreamChunk::ToolCallResult {
        id: "call_done".into(),
        result: "UNIQUE-RESULT-2".into(),
    }));
    items.extend(call("call_running", "2 + 2"));
    items.push(stalled());

    let (_, outcome) = run_scripted_turn(&mut session, "Add up the numbers.", items).await;
    assert_eq!(outcome, TurnOutcome::Persisted);
    let (_, outcome) = run_turn(&mut session, "continue").await;
    assert_eq!(outcome, TurnOutcome::Persisted);

    let requests = daemon.requests();
    assert_eq!(requests.len(), 1);
    let sent = messages(&requests[0]);
    assert_valid_order(&sent);
    let dump = serde_json::to_string_pretty(&sent).unwrap();
    let calls: Vec<String> = sent.iter().flat_map(call_ids).collect();
    assert_eq!(calls, vec!["call_done".to_string()], "{dump}");
    assert!(dump.contains("UNIQUE-RESULT-2"), "{dump}");
    assert!(!dump.contains("call_running"), "{dump}");
}

/// A turn that stalls before any output keeps its user message, so the
/// "continue" request has the task in context, with a note as its reply.
#[tokio::test]
async fn an_empty_stalled_turn_keeps_its_user_message() {
    let daemon = FakeDaemon::scripted(Script::new().route(MODEL, [Reply::text("Resumed.")]));
    let (mut session, _workspace) = session_on(&daemon).await;

    let (events, outcome) =
        run_scripted_turn(&mut session, "Count the files in /data", vec![stalled()]).await;
    assert!(stall_error(&events));
    assert_eq!(outcome, TurnOutcome::Persisted, "not rolled back");
    let history = session.conversation().unwrap().messages();
    assert_eq!(history.len(), 2, "{history:?}");
    let note = kept_failed_turn_note(StreamErrorKind::Stalled).unwrap();
    assert!(matches!(history[1], Message::Assistant(_)), "{history:?}");
    assert!(format!("{:?}", history[1]).contains(note), "{history:?}");

    run_turn(&mut session, "continue").await;
    let sent = messages(&daemon.requests()[0]);
    assert_valid_order(&sent);
    let dump = serde_json::to_string_pretty(&sent).unwrap();
    assert_eq!(sent.len(), 3, "{dump}");
    assert_eq!(content_text(&sent[0]), "Count the files in /data", "{dump}");
    assert_eq!(content_text(&sent[1]), note, "{dump}");
    assert_eq!(content_text(&sent[2]), "continue", "{dump}");
}

/// A cancelled empty turn is still rolled back (AGE-243): only a turn an
/// error ended is kept.
#[tokio::test]
async fn an_empty_cancelled_turn_is_still_rolled_back() {
    let daemon = FakeDaemon::scripted(Script::new());
    let (mut session, _workspace) = session_on(&daemon).await;
    let items = vec![ScriptedItem::CancelThen(StreamChunk::ApiCallUsage(
        crate::models::token_usage::ApiCallUsage::default(),
    ))];
    let (_, outcome) = run_scripted_turn(&mut session, "do it", items).await;
    assert_eq!(
        outcome,
        TurnOutcome::DroppedAndRolledBack("do it".to_string())
    );
    assert!(session.conversation().unwrap().messages().is_empty());
}

/// A connection failure keeps its turn too, with a fixed note: the error's
/// own text — here a fake key and a request URL — never reaches the
/// history, so it is neither saved nor sent back to the provider.
#[tokio::test]
async fn a_kept_turn_never_persists_the_error_text() {
    const SECRET: &str = "sk-FAKE-1438-do-not-persist";
    const URL: &str = "https://api.example.test/v1/chat/completions?key=";
    let daemon = FakeDaemon::scripted(Script::new().route(MODEL, [Reply::text("Resumed.")]));
    let (mut session, _workspace) = session_on(&daemon).await;
    let error = ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
        StreamErrorKind::Transport,
        format!("error sending request for url ({URL}{SECRET}): body {{\"key\":\"{SECRET}\"}}"),
    )));

    let (_, outcome) = run_scripted_turn(&mut session, "Fix the bug.", vec![error]).await;
    assert_eq!(outcome, TurnOutcome::Persisted);
    let saved = serde_json::to_string(&session.conversation().unwrap().messages()).unwrap();
    assert!(!saved.contains(SECRET) && !saved.contains(URL), "{saved}");
    assert!(
        saved.contains(kept_failed_turn_note(StreamErrorKind::Transport).unwrap()),
        "{saved}"
    );

    run_turn(&mut session, "continue").await;
    let body = String::from_utf8_lossy(&daemon.requests()[0].body).into_owned();
    assert!(!body.contains(SECRET) && !body.contains(URL), "{body}");
    assert_valid_order(&messages(&daemon.requests()[0]));
}

/// An error that is not a stall or a connection failure rolls the empty
/// turn back exactly as before (AGE-243), and keeps none of its
/// round-trips: here an auth rejection after a tool call.
#[tokio::test]
async fn an_auth_error_still_rolls_back() {
    let daemon = FakeDaemon::scripted(Script::new());
    let (mut session, _workspace) = session_on(&daemon).await;
    let auth = || {
        ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
            StreamErrorKind::Auth,
            "401 Unauthorized",
        )))
    };

    let (_, outcome) = run_scripted_turn(&mut session, "do it", vec![auth()]).await;
    assert_eq!(
        outcome,
        TurnOutcome::DroppedAndRolledBack("do it".to_string())
    );
    assert!(session.conversation().unwrap().messages().is_empty());

    let items = vec![
        ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
            id: "call_1".into(),
            name: "calculate".into(),
        }),
        ScriptedItem::Chunk(StreamChunk::ToolCallResult {
            id: "call_1".into(),
            result: "UNIQUE-AUTH-RESULT".into(),
        }),
        auth(),
    ];
    run_scripted_turn(&mut session, "do it", items).await;
    let history = format!("{:?}", session.conversation().unwrap().messages());
    assert!(!history.contains("UNIQUE-AUTH-RESULT"), "{history}");
    assert!(!history.contains("ended early"), "{history}");
}
