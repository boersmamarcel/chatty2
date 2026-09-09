//! `AgentSession` exercised with no frontend crate in the dependency graph
//! (AGE-194 acceptance), plus the session's own event goldens.
//!
//! The goldens under `session/goldens/` are the `SessionEvent` contract for
//! each scripted scenario. The frontends' adapter characterizations replay
//! the same scenarios through `From<SessionEvent>` and compare against their
//! own Phase 0 goldens; a change here that they do not expect shows up there.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use rig_core::completion::Message;
use rig_core::message::UserContent;

use super::*;
use crate::factories::agent_factory::{AgentBuildContext, AgentServices};
use crate::services::RecoveryAction;
use crate::services::llm_service::StreamChunk;
use crate::services::{Scenario, ScriptedItem, assert_golden, clarification_scenario, scenarios};
use crate::settings::models::models_store::ModelConfig;
use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
use crate::tools::invoke_agent_tool::InvokeAgentProgress;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/session/goldens")
}

fn config() -> AgentSessionConfig {
    AgentSessionConfig {
        execution_settings: ExecutionSettingsModel::default(),
        surface: StreamSurface::InteractiveTui,
        loop_guard: false,
    }
}

fn policy() -> TurnPolicy {
    TurnPolicy {
        surface: StreamSurface::InteractiveTui,
        max_agent_turns: 10,
        loop_guard: false,
        already_asked_to_retry: false,
    }
}

/// A session around a real, network-free `Conversation`: Ollama client
/// construction is purely local, and the agent is built against the
/// session's own store handles, as a frontend would.
async fn session_with_conversation() -> AgentSession {
    // Agent construction resolves the MCP repository (for the always-on
    // list_mcp tool); `init_repositories()` only resolves paths and is a
    // no-op after the first call.
    let _ = crate::init_repositories();

    let mut session = AgentSession::new(config());
    let handles = session.approval_handles();
    let model_config = ModelConfig::new(
        "m1".to_string(),
        "Test Model".to_string(),
        ProviderType::Ollama,
        "llama3.2".to_string(),
    );
    let provider_config = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
    let conversation = Conversation::new(
        "c1".to_string(),
        "New Chat".to_string(),
        &model_config,
        &provider_config,
        AgentBuildContext {
            pending_approvals: Some(handles.pending_approvals),
            pending_clarifications: Some(handles.pending_clarifications),
            pending_write_approvals: Some(handles.pending_write_approvals),
            ..AgentBuildContext::from_services(AgentServices::default())
        },
    )
    .await
    .expect("conversation should build without network access");
    session.set_conversation(Some(conversation));
    session
}

fn scenario(name: &str) -> Scenario {
    scenarios()
        .into_iter()
        .chain([clarification_scenario()])
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("scenario {name} exists"))
}

/// Run a scripted turn on `session` and return every event, in order.
async fn run_turn(
    session: &mut AgentSession,
    input: TurnInput,
    scenario: Scenario,
) -> Vec<SessionEvent> {
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    let turn = session
        .begin_scripted_turn(input, scenario, move |event| sink.borrow_mut().push(event))
        .expect("turn starts");
    assert!(session.is_turn_active());
    turn.await;
    Rc::try_unwrap(events).unwrap().into_inner()
}

fn user_text(message: &Message) -> Option<String> {
    match message {
        Message::User { content } => Some(extract_user_text(content)),
        _ => None,
    }
}

#[tokio::test]
async fn a_scripted_turn_runs_end_to_end_and_persists_its_reply() {
    let mut session = session_with_conversation().await;

    let events = run_turn(
        &mut session,
        TurnInput::text("what is this?"),
        scenario("tool_call_then_result"),
    )
    .await;

    assert!(matches!(events.first(), Some(SessionEvent::TurnStarted)));
    assert!(matches!(events.last(), Some(SessionEvent::TurnEnded)));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCallResult { id, .. } if id == "call-1"))
    );

    for event in &events {
        session.apply(event);
    }
    // The turn is still open until the owner finishes it; the reply so far is
    // on the conversation, not in history.
    assert!(session.is_turn_active());
    assert_eq!(
        session
            .conversation()
            .unwrap()
            .streaming_message()
            .map(String::as_str),
        Some("It is the readme.")
    );

    let outcome = session
        .finish_turn(None, vec![])
        .expect("has a conversation");
    assert!(matches!(outcome, TurnOutcome::Persisted));
    assert!(!session.is_turn_active());
    assert!(
        session.finish_turn(None, vec![]).is_none(),
        "finishing the same turn twice commits once"
    );

    let history = session.conversation().unwrap().messages();
    assert_eq!(history.len(), 2);
    assert_eq!(user_text(&history[0]).as_deref(), Some("what is this?"));
    assert!(matches!(history[1], Message::Assistant { .. }));
    assert!(session.should_generate_title());
}

#[tokio::test]
async fn usage_is_folded_from_per_call_records_and_recorded_on_finish() {
    let mut session = session_with_conversation().await;
    let events = run_turn(
        &mut session,
        TurnInput::text("hi"),
        scenario("token_usage_on_done"),
    )
    .await;
    for event in &events {
        session.apply(event);
    }
    let usage = session.last_turn_usage().expect("the aggregate arrived");
    assert_eq!(usage.input_tokens, 234);
    assert_eq!(usage.cache_read_tokens, 1000);
    assert_eq!(usage.api_turn_count, 1);
    assert_eq!(usage.calls.len(), 1);

    session.finish_turn(None, vec![]);
    let conversation = session.conversation().unwrap();
    assert_eq!(conversation.token_usage().total_input_tokens, 234);
    assert!(
        session.last_turn_usage().is_none(),
        "recorded once, on the turn it belongs to"
    );
}

/// The todo protocol's other nudge: a plan whose todos are all done but
/// never verified asks for `verify_completion` once the turn ends.
#[tokio::test]
async fn a_finished_plan_without_verification_is_nudged_after_the_turn() {
    use crate::services::AgentTodoStatus;

    let controller = AgentTaskController::new();
    controller
        .write_todos(
            "Ship".into(),
            vec![("t1".into(), "Implement".into(), "Implement change".into())],
        )
        .unwrap();
    controller
        .update_todo("t1".into(), AgentTodoStatus::Done, None, None)
        .unwrap();

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let stream = crate::services::scripted_stream(scenario("text_only").items, cancel_flag.clone());
    let events: Rc<RefCell<Vec<SessionEvent>>> = Rc::default();
    let sink = events.clone();
    drive(
        stream,
        Vec::new(),
        controller,
        cancel_flag,
        Arc::new(parking_lot::Mutex::new(None)),
        policy(),
        move |event| sink.borrow_mut().push(event),
    )
    .await;

    let events = events.borrow();
    assert!(matches!(
        events.last(),
        Some(SessionEvent::FollowUp(prompt)) if prompt.contains("verify_completion")
    ));
}

#[tokio::test]
async fn a_cancelled_turn_with_no_text_rolls_back_the_user_message() {
    let mut session = session_with_conversation().await;
    let events = run_turn(
        &mut session,
        TurnInput::text("do it"),
        Scenario {
            name: "cancel_before_text",
            progress: Vec::new(),
            items: vec![
                // A tool call would put an item in the trace, which counts as
                // content under D4; usage does not.
                ScriptedItem::CancelThen(StreamChunk::ApiCallUsage(
                    crate::models::token_usage::ApiCallUsage::default(),
                )),
                ScriptedItem::Chunk(StreamChunk::Text("never".into())),
            ],
        },
    )
    .await;

    let tail: Vec<&SessionEvent> = events.iter().rev().take(2).collect();
    assert!(matches!(tail[0], SessionEvent::TurnEnded));
    assert!(matches!(tail[1], SessionEvent::Cancelled));
    assert!(!events.iter().any(|e| matches!(e, SessionEvent::Text(_))));

    for event in &events {
        session.apply(event);
    }
    let outcome = session.finish_turn(None, vec![]).unwrap();
    assert!(matches!(outcome, TurnOutcome::DroppedAndRolledBack(ref text) if text == "do it"));
    assert!(session.conversation().unwrap().messages().is_empty());
}

#[tokio::test]
async fn a_second_turn_is_refused_while_one_is_running() {
    let mut session = session_with_conversation().await;
    let turn = session
        .begin_scripted_turn(TurnInput::text("one"), scenario("text_only"), |_| {})
        .expect("first turn starts");
    let second = session.begin_scripted_turn(TurnInput::text("two"), scenario("text_only"), |_| {});
    assert!(second.is_err(), "a turn is already running");
    // The refused turn left no trace: only the first message was committed.
    assert_eq!(session.conversation().unwrap().messages().len(), 1);
    turn.await;
}

#[tokio::test]
async fn a_regenerate_turn_adds_nothing_to_history() {
    let mut session = session_with_conversation().await;
    let conversation = session.conversation_mut().unwrap();
    conversation.add_user_message_with_attachments(
        Message::User {
            content: vec![UserContent::text("again?".to_string())],
        },
        vec![],
    );
    let before = conversation.messages();

    let events = run_turn(&mut session, TurnInput::regenerate(), scenario("text_only")).await;
    for event in &events {
        session.apply(event);
    }
    assert_eq!(session.conversation().unwrap().messages(), before);

    session.finish_turn(None, vec![]);
    assert_eq!(
        session.conversation().unwrap().messages().len(),
        before.len() + 1
    );
}

/// AGE-274: the session records the turn's trace from its own events, and
/// persists it with the reply when the owner has no trace of its own.
#[tokio::test]
async fn a_tool_call_turn_persists_its_trace_without_a_frontend() {
    let mut session = session_with_conversation().await;
    let events = run_turn(
        &mut session,
        TurnInput::text("what is this?"),
        scenario("tool_call_then_result"),
    )
    .await;
    for event in &events {
        session.apply(event);
    }
    let trace = session.trace_json().expect("the tool call is in the trace");
    assert_eq!(trace["items"].as_array().map(Vec::len), Some(1));

    session.finish_turn(None, vec![]);
    let conversation = session.conversation().unwrap();
    let entry = conversation.entries().last().expect("the reply");
    let persisted = entry
        .system_trace
        .as_ref()
        .expect("trace persisted with the reply");
    let item = &persisted["items"][0];
    assert_eq!(item["ToolCall"]["tool_name"], "read_file");
    assert_eq!(item["ToolCall"]["input"], r#"{"path":"README.md"}"#);
    assert_eq!(item["ToolCall"]["output"], "# Chatty");
    assert!(
        conversation.streaming_trace().is_none(),
        "cleared with the turn"
    );
    assert!(session.trace_json().is_none());
}

/// AGE-274: an approval and a clarification are in the trace too.
#[tokio::test]
async fn approvals_and_clarifications_are_in_the_trace() {
    let mut session = session_with_conversation().await;
    let events = run_turn(
        &mut session,
        TurnInput::text("go"),
        scenario("approval_denied"),
    )
    .await;
    for event in &events {
        session.apply(event);
    }
    let trace = session.trace_json().unwrap();
    let items = trace["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "the tool call and its approval");
    assert_eq!(items[1]["ApprovalPrompt"]["state"], "Denied");
    session.finish_turn(None, vec![]);

    let events = run_turn(
        &mut session,
        TurnInput::text("deploy?"),
        scenario("clarification_requested"),
    )
    .await;
    for event in &events {
        session.apply(event);
    }
    let trace = session.trace_json().unwrap();
    assert_eq!(trace["items"][0]["ClarificationPrompt"]["state"], "Pending");
}

/// AGE-273: the recovery budget is per error kind, from the shared policy
/// table, and starts over with the next human turn.
#[tokio::test]
async fn recovery_budget_runs_out_per_kind_and_resets_on_a_human_turn() {
    use std::time::Duration;

    let mut session = session_with_conversation().await;
    session.set_config(AgentSessionConfig {
        surface: StreamSurface::Headless,
        ..config()
    });
    let transport = StreamError::new(StreamErrorKind::Transport, "reset");
    let rate_limited = StreamError::new(StreamErrorKind::RateLimited, "429");

    for attempt in 0..crate::services::HEADLESS_TRANSPORT_RETRY_ATTEMPTS {
        assert_eq!(
            session.recovery_action(&transport),
            RecoveryAction::Retry {
                after: Duration::from_secs(10 * (attempt as u64 + 1))
            }
        );
    }
    assert_eq!(session.recovery_action(&transport), RecoveryAction::Stop);
    assert!(
        matches!(
            session.recovery_action(&rate_limited),
            RecoveryAction::Retry { .. }
        ),
        "another kind has its own budget"
    );
    assert_eq!(
        session.recovery_action(&StreamError::new(StreamErrorKind::Other, "?")),
        RecoveryAction::Stop
    );

    let events = run_turn(
        &mut session,
        TurnInput::text("again"),
        scenario("text_only"),
    )
    .await;
    for event in &events {
        session.apply(event);
    }
    session.finish_turn(None, vec![]);
    assert!(
        matches!(
            session.recovery_action(&transport),
            RecoveryAction::Retry { .. }
        ),
        "a human turn starts a new budget"
    );
}

/// Two sessions in one process: separate stores, separate turns, and a
/// request raised on one never reaches the other (the point of AGE-193).
#[tokio::test]
async fn two_sessions_share_nothing() {
    let mut a = session_with_conversation().await;
    let mut b = session_with_conversation().await;

    let handles_a = a.approval_handles();
    let handles_b = b.approval_handles();
    assert!(!Arc::ptr_eq(
        &handles_a.pending_approvals,
        &handles_b.pending_approvals
    ));
    assert!(!Arc::ptr_eq(
        &handles_a.pending_write_approvals,
        &handles_b.pending_write_approvals
    ));
    assert!(!Arc::ptr_eq(
        &handles_a.pending_clarifications,
        &handles_b.pending_clarifications
    ));

    let events_a = run_turn(&mut a, TurnInput::text("a"), scenario("text_only")).await;
    assert!(a.is_turn_active());
    assert!(!b.is_turn_active(), "a turn on A is not a turn on B");
    let events_b = run_turn(&mut b, TurnInput::text("b"), scenario("tool_error")).await;

    assert!(
        !events_a
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCallError { .. }))
    );
    assert!(
        events_b
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCallError { .. }))
    );
}

#[tokio::test]
async fn a_todo_nudge_follows_the_turn_it_belongs_to() {
    let events = replay_scenario(scenario("todo_nudge_mid_stream"), policy()).await;
    let ended = events
        .iter()
        .position(|e| matches!(e, SessionEvent::TurnEnded))
        .expect("the turn ended");
    let follow_up = events
        .iter()
        .position(|e| matches!(e, SessionEvent::FollowUp(p) if p.contains("write_todos")))
        .expect("the todo protocol asked for a plan");
    assert!(follow_up > ended, "the follow-up comes after TurnEnded");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::ToolCallResult { id, .. } if id == "call-2")),
        "the result that triggered the nudge is still delivered"
    );
}

#[tokio::test]
async fn a_malformed_tool_call_is_retried_once() {
    let malformed = || Scenario {
        name: "malformed_tool_call",
        progress: Vec::new(),
        items: vec![ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
            StreamErrorKind::MalformedToolCall,
            "invalid JSON",
        )))],
    };

    let first = replay_scenario(malformed(), policy()).await;
    assert!(
        first
            .iter()
            .any(|e| matches!(e, SessionEvent::FollowUp(p) if p == MALFORMED_TOOL_CALL_FOLLOW_UP))
    );

    let retried = replay_scenario(
        malformed(),
        TurnPolicy {
            already_asked_to_retry: true,
            ..policy()
        },
    )
    .await;
    assert!(
        !retried
            .iter()
            .any(|e| matches!(e, SessionEvent::FollowUp(_))),
        "the retry is bounded to one attempt"
    );
}

/// The bound holds through `prepare_turn`: the nudge turn is the one that
/// carries the follow-up text, so it is the one that must not nudge again.
#[tokio::test]
async fn the_retry_turn_itself_does_not_nudge_again() {
    let malformed = || Scenario {
        name: "malformed_tool_call",
        progress: Vec::new(),
        items: vec![ScriptedItem::Chunk(StreamChunk::Error(StreamError::new(
            StreamErrorKind::MalformedToolCall,
            "invalid JSON",
        )))],
    };
    let mut session = session_with_conversation().await;

    let first = run_turn(&mut session, TurnInput::text("do it"), malformed()).await;
    let nudge = first.iter().find_map(|e| match e {
        SessionEvent::FollowUp(prompt) => Some(prompt.clone()),
        _ => None,
    });
    assert_eq!(nudge.as_deref(), Some(MALFORMED_TOOL_CALL_FOLLOW_UP));
    session.finish_turn(None, vec![]);

    let retried = run_turn(
        &mut session,
        TurnInput::protocol_follow_up(nudge.unwrap()),
        malformed(),
    )
    .await;
    assert!(
        !retried
            .iter()
            .any(|e| matches!(e, SessionEvent::FollowUp(_))),
        "the retry turn is the last one"
    );
}

/// The retry is bounded by spotting this text in history, and hidden from
/// the transcript by the same prefix. Both depend on the matcher recognising
/// it — if the text drifts, the retry silently becomes unbounded and visible
/// at once.
#[test]
fn retry_follow_up_is_recognised_as_a_protocol_nudge() {
    assert!(crate::services::is_protocol_follow_up_text(
        MALFORMED_TOOL_CALL_FOLLOW_UP
    ));
}

#[test]
fn retry_follow_up_explains_what_to_do_differently() {
    assert!(MALFORMED_TOOL_CALL_FOLLOW_UP.contains("malformed or truncated"));
    assert!(MALFORMED_TOOL_CALL_FOLLOW_UP.contains("smaller steps"));
}

/// One line per event, in a shape that reads as a diff.
fn describe(event: &SessionEvent) -> String {
    match event {
        SessionEvent::TurnStarted => "TurnStarted".to_string(),
        SessionEvent::Text(text) => format!("Text({text:?})"),
        SessionEvent::ToolCallStarted { id, name } => {
            format!("ToolCallStarted(id={id:?}, {name:?})")
        }
        SessionEvent::ToolCallInput { id, arguments } => {
            format!("ToolCallInput(id={id:?}, {arguments:?})")
        }
        SessionEvent::ToolCallResult { id, result } => {
            format!("ToolCallResult(id={id:?}, {result:?})")
        }
        SessionEvent::ToolCallError { id, error } => format!("ToolCallError(id={id:?}, {error:?})"),
        SessionEvent::ApprovalRequested {
            id,
            command,
            is_sandboxed,
        } => format!("ApprovalRequested(id={id:?}, {command:?}, sandboxed={is_sandboxed})"),
        SessionEvent::ApprovalResolved { id, approved } => {
            format!("ApprovalResolved(id={id:?}, approved={approved})")
        }
        SessionEvent::ClarificationRequested { id, questions } => {
            let texts: Vec<&str> = questions.iter().map(|q| q.question.as_str()).collect();
            format!("ClarificationRequested(id={id:?}, {texts:?})")
        }
        SessionEvent::ApiCallUsage(call) => format!(
            "ApiCallUsage(turn={}, in={}, out={}, cache_read={}, cache_write={})",
            call.turn,
            call.input_tokens,
            call.output_tokens,
            call.cache_read_tokens,
            call.cache_write_tokens
        ),
        SessionEvent::TokenUsage(u) => format!(
            "TokenUsage(in={}, out={}, cache_read={}, cache_write={}, api_turns={}, calls={})",
            u.input_tokens,
            u.output_tokens,
            u.cache_read_tokens,
            u.cache_write_tokens,
            u.api_turn_count,
            u.calls.len()
        ),
        SessionEvent::TurnMessages(messages) => format!("TurnMessages(len={})", messages.len()),
        SessionEvent::Delegation(progress) => match progress {
            InvokeAgentProgress::Started {
                agent_name, prompt, ..
            } => {
                format!("Delegation(Started {agent_name:?}, {prompt:?})")
            }
            InvokeAgentProgress::Text(text) => format!("Delegation(Text {text:?})"),
            InvokeAgentProgress::Finished { success, result } => {
                format!("Delegation(Finished success={success}, {result:?})")
            }
        },
        SessionEvent::Error(error) => format!("Error(kind={:?}, {:?})", error.kind, error.message),
        SessionEvent::Cancelled => "Cancelled".to_string(),
        SessionEvent::TurnEnded => "TurnEnded".to_string(),
        SessionEvent::FollowUp(prompt) => {
            let kind = if prompt.contains("write_todos") {
                "write_todos"
            } else if prompt.contains("verify_completion") {
                "verify_completion"
            } else {
                "other"
            };
            format!("FollowUp({kind})")
        }
    }
}

#[tokio::test]
async fn session_events_match_goldens() {
    let dir = goldens_dir();
    for scenario in scenarios().into_iter().chain([clarification_scenario()]) {
        let name = scenario.name;
        let events: Vec<String> = replay_scenario(scenario, policy())
            .await
            .iter()
            .map(describe)
            .collect();
        assert_golden(&dir, name, &events);
    }
}

/// A hosted turn is recorded by the local session exactly like a local one
/// (AGE-298): the user message is committed when the turn opens, and the
/// reply the wire carries back is finalized into the conversation. Without
/// `open_hosted_turn` the session never knows a turn ran, `finish_turn` finds
/// nothing to finish, and the reply is dropped — which is how a conversation
/// taken online came back with none of the turns it ran there.
#[tokio::test]
async fn a_hosted_turn_is_recorded_by_the_local_session() {
    let mut session = session_with_conversation().await;
    let flag = Arc::new(AtomicBool::new(false));
    session
        .open_hosted_turn(&TurnInput::text("what did we discuss"), flag)
        .expect("the turn opens");
    assert!(
        session.is_turn_active(),
        "the session knows a turn is running"
    );
    assert!(
        session
            .open_hosted_turn(&TurnInput::text("again"), Arc::new(AtomicBool::new(false)))
            .is_err(),
        "a second turn is refused while one runs"
    );

    for event in [
        SessionEvent::TurnStarted,
        SessionEvent::Text("we talked ".into()),
        SessionEvent::Text("about the weather".into()),
        SessionEvent::TurnEnded,
    ] {
        session.apply(&event);
    }
    assert!(
        session.finish_turn(None, Vec::new()).is_some(),
        "the turn finalizes like a local one"
    );
    assert!(!session.is_turn_active());

    let messages = session.conversation().unwrap().messages();
    assert_eq!(messages.len(), 2, "user message and reply: {messages:?}");
    assert!(matches!(messages[0], Message::User { .. }));
    assert!(matches!(messages[1], Message::Assistant { .. }));
}
