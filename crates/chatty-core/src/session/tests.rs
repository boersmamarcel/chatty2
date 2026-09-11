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

/// A network-free model: Ollama client construction is purely local.
fn unpriced_model() -> ModelConfig {
    ModelConfig::new(
        "m1".to_string(),
        "Test Model".to_string(),
        ProviderType::Ollama,
        "llama3.2".to_string(),
    )
}

fn ollama_provider() -> ProviderConfig {
    ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama)
}

/// A session around a real, network-free `Conversation`: Ollama client
/// construction is purely local, and the agent is built against the
/// session's own store handles, as a frontend would.
async fn session_with_conversation() -> AgentSession {
    session_with_model(&unpriced_model()).await
}

/// [`session_with_conversation`] on a given model (its prices decide what a
/// turn costs, AGE-351).
async fn session_with_model(model_config: &ModelConfig) -> AgentSession {
    // Agent construction resolves the MCP repository (for the always-on
    // list_mcp tool); `init_repositories()` only resolves paths and is a
    // no-op after the first call.
    let _ = crate::init_repositories();

    let mut session = AgentSession::new(config());
    let handles = session.approval_handles();
    let conversation = Conversation::new(
        "c1".to_string(),
        "New Chat".to_string(),
        model_config,
        &ollama_provider(),
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

// ── Conversation totals and pricing at the turn barrier (AGE-351) ────────────

mod totals_and_pricing {
    use super::*;
    use crate::models::token_usage::ApiCallUsage;
    use crate::repositories::{ConversationRepository, ConversationSqliteRepository};

    /// A priced model, with the cache rates set so all four prices are
    /// exercised: $3/M input, $15/M output, $0.30/M cache read, $3.75/M
    /// cache write.
    fn priced_model() -> ModelConfig {
        let mut model = unpriced_model();
        model.cost_per_million_input_tokens = Some(3.0);
        model.cost_per_million_output_tokens = Some(15.0);
        model.cost_per_million_cache_read_tokens = Some(0.3);
        model.cost_per_million_cache_write_tokens = Some(3.75);
        model
    }

    fn call(turn: u32, input: u32, read: u32, write: u32, output: u32) -> ApiCallUsage {
        ApiCallUsage {
            turn,
            input_tokens: input,
            cache_read_tokens: read,
            cache_write_tokens: write,
            output_tokens: output,
        }
    }

    /// A turn with `tool_calls` tool round-trips, then a reply, then the
    /// per-call usage records and the provider's aggregate, as rig emits
    /// them.
    fn tool_turn(tool_calls: usize, calls: Vec<ApiCallUsage>) -> Scenario {
        let mut items = Vec::new();
        for i in 0..tool_calls {
            let id = format!("call-{i}");
            items.push(ScriptedItem::Chunk(StreamChunk::ToolCallStarted {
                id: id.clone(),
                name: "read_file".into(),
            }));
            items.push(ScriptedItem::Chunk(StreamChunk::ToolCallResult {
                id,
                result: "ok".into(),
            }));
        }
        items.push(ScriptedItem::Chunk(StreamChunk::Text("Done.".into())));
        let aggregate = TokenUsage::from_calls(calls.clone());
        for call in calls {
            items.push(ScriptedItem::Chunk(StreamChunk::ApiCallUsage(call)));
        }
        items.push(ScriptedItem::Chunk(StreamChunk::TurnUsage(ApiCallUsage {
            turn: 0,
            input_tokens: aggregate.input_tokens,
            cache_read_tokens: aggregate.cache_read_tokens,
            cache_write_tokens: aggregate.cache_write_tokens,
            output_tokens: aggregate.output_tokens,
        })));
        items.push(ScriptedItem::Chunk(StreamChunk::Done));
        Scenario {
            name: "tool_turn",
            progress: Vec::new(),
            items,
        }
    }

    /// Run a scripted turn to completion and finish it, as an owner would.
    async fn complete_turn(session: &mut AgentSession, scenario: Scenario) -> Vec<SessionEvent> {
        let events = run_turn(session, TurnInput::text("go"), scenario).await;
        for event in &events {
            session.apply(event);
        }
        assert!(matches!(
            session.finish_turn(None, vec![]),
            Some(TurnOutcome::Persisted)
        ));
        events
    }

    /// Three turns with two, one and one tool calls: the four calls are on
    /// the lifetime total, the token total is the sum of the turns' usage,
    /// and the context fill is the last call's prompt — not the sum of the
    /// turn's requests, and not the output.
    async fn three_turns_four_tool_calls(session: &mut AgentSession) {
        complete_turn(
            session,
            tool_turn(
                2,
                vec![call(1, 1_000, 0, 2_000, 100), call(2, 200, 2_000, 0, 400)],
            ),
        )
        .await;
        complete_turn(session, tool_turn(1, vec![call(1, 300, 2_000, 0, 50)])).await;
        complete_turn(
            session,
            tool_turn(
                1,
                vec![call(1, 400, 2_000, 100, 60), call(2, 50, 2_100, 0, 20)],
            ),
        )
        .await;
    }

    #[tokio::test]
    async fn three_turns_and_four_tool_calls_report_the_totals() {
        let mut session = session_with_conversation().await;
        three_turns_four_tool_calls(&mut session).await;

        let conversation = session.conversation().unwrap();
        assert_eq!(conversation.tool_call_count(), 4);
        let usage = conversation.token_usage();
        assert_eq!(usage.message_usages.len(), 3, "one usage per turn");
        assert_eq!(
            usage.total_input_tokens,
            usage
                .message_usages
                .iter()
                .map(|u| u.input_tokens)
                .sum::<u32>()
        );
        assert_eq!(usage.total_input_tokens, 1_000 + 200 + 300 + 400 + 50);
        assert_eq!(usage.total_output_tokens, 100 + 400 + 50 + 60 + 20);
        assert_eq!(usage.total_cache_read_tokens, 2_000 + 2_000 + 2_000 + 2_100);
        assert_eq!(usage.total_cache_write_tokens, 2_000 + 100);
        assert_eq!(
            conversation.context_tokens(),
            50 + 2_100,
            "the last call's prompt: input + cache read + cache write, no output"
        );
    }

    /// The numbers chatty-gpui's `price_usage` produced for this usage on
    /// this model before pricing moved into the session — hand-computed from
    /// the same formula it applied (`TokenUsage::calculate_cost` with the
    /// model's four prices): 1_200 input × $3/M + 500 output × $15/M +
    /// 2_000 cache read × $0.30/M + 2_000 cache write × $3.75/M.
    const GPUI_COST_FOR_FIXED_USAGE: f64 = 0.0036 + 0.0075 + 0.0006 + 0.0075;

    #[tokio::test]
    async fn a_priced_model_costs_the_turn_in_the_session() {
        let mut session = session_with_model(&priced_model()).await;
        complete_turn(
            &mut session,
            tool_turn(
                1,
                vec![call(1, 1_000, 0, 2_000, 100), call(2, 200, 2_000, 0, 400)],
            ),
        )
        .await;

        let usage = session.conversation().unwrap().token_usage();
        let turn_cost = usage.message_usages[0]
            .estimated_cost_usd
            .expect("a priced model costs the turn");
        assert!(turn_cost > 0.0);
        assert!(
            (turn_cost - GPUI_COST_FOR_FIXED_USAGE).abs() < 1e-12,
            "session cost {turn_cost} must equal what gpui computed ({GPUI_COST_FOR_FIXED_USAGE})"
        );
        assert!((usage.total_estimated_cost_usd - GPUI_COST_FOR_FIXED_USAGE).abs() < 1e-12);
    }

    #[tokio::test]
    async fn a_model_without_prices_leaves_the_cost_none_and_the_total_unchanged() {
        let mut session = session_with_conversation().await;
        complete_turn(&mut session, tool_turn(1, vec![call(1, 1_000, 0, 0, 100)])).await;

        let usage = session.conversation().unwrap().token_usage();
        assert_eq!(usage.message_usages[0].estimated_cost_usd, None);
        assert_eq!(usage.total_estimated_cost_usd, 0.0);

        // Half a price is no price: the same rule gpui applied.
        let mut input_only = unpriced_model();
        input_only.cost_per_million_input_tokens = Some(3.0);
        let mut session = session_with_model(&input_only).await;
        complete_turn(&mut session, tool_turn(0, vec![call(1, 1_000, 0, 0, 100)])).await;
        let usage = session.conversation().unwrap().token_usage();
        assert_eq!(usage.message_usages[0].estimated_cost_usd, None);
        assert_eq!(usage.total_estimated_cost_usd, 0.0);
    }

    /// A model switch installs the new model's prices with its agent, so the
    /// next turn is costed at the new rates.
    #[tokio::test]
    async fn a_model_switch_switches_the_prices() {
        let mut session = session_with_conversation().await;
        assert!(session.conversation().unwrap().pricing().is_none());

        let ctx = session.build_context(AgentBuildContext::from_services(AgentServices::default()));
        let built = crate::factories::AgentClient::from_model_config_with_tools(
            &priced_model(),
            &ollama_provider(),
            ctx,
        )
        .await
        .expect("the agent builds without network access");
        assert!(session.install_agent(built, &priced_model(), None));
        assert_eq!(session.conversation().unwrap().model_id(), "m1");
        assert_eq!(
            session.conversation().unwrap().pricing(),
            priced_model().token_pricing().as_ref()
        );

        complete_turn(
            &mut session,
            tool_turn(0, vec![call(1, 1_000_000, 0, 0, 0)]),
        )
        .await;
        let usage = session.conversation().unwrap().token_usage();
        assert!((usage.total_estimated_cost_usd - 3.0).abs() < 1e-9);
    }

    /// "Reloading the app reports the same numbers": the totals go through
    /// the row and come back on a restored conversation. A headless GPUI app
    /// cannot be driven here, so this is the store round trip the app does
    /// on open — save, reopen the store, restore the conversation.
    #[tokio::test]
    async fn the_totals_survive_a_store_round_trip() {
        let mut session = session_with_model(&priced_model()).await;
        three_turns_four_tool_calls(&mut session).await;
        let conversation = session.conversation().unwrap();
        let expected = (
            conversation.tool_call_count(),
            conversation.context_tokens(),
            conversation.token_usage().total_estimated_cost_usd,
        );
        assert!(expected.2 > 0.0);
        let data = conversation
            .to_conversation_data()
            .expect("the row serializes");
        assert_eq!((data.tool_call_count, data.context_tokens), (4, 2_150));

        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("conversations.db");
        ConversationSqliteRepository::deferred_with_path(db_path.clone())
            .save("c1", data)
            .await
            .expect("save");

        // A fresh handle, as a restarted app would open.
        let reopened = ConversationSqliteRepository::deferred_with_path(db_path);
        let metadata = reopened.load_metadata().await.expect("load_metadata");
        assert_eq!(
            (
                metadata[0].tool_call_count,
                metadata[0].context_tokens,
                metadata[0].total_cost
            ),
            expected,
            "the sidebar layer reports the same numbers"
        );
        let row = reopened
            .load_one("c1")
            .await
            .expect("load_one")
            .expect("the row exists");

        let mut restored = AgentSession::new(config());
        restored
            .restore_conversation(
                row,
                &priced_model(),
                &ollama_provider(),
                AgentBuildContext::from_services(AgentServices::default()),
            )
            .await
            .expect("the conversation restores");
        let conversation = restored.conversation().unwrap();
        assert_eq!(
            (
                conversation.tool_call_count(),
                conversation.context_tokens(),
                conversation.token_usage().total_estimated_cost_usd
            ),
            expected,
            "the restored conversation reports the same numbers"
        );
    }

    /// A row from before the totals existed restores with both at 0.
    #[tokio::test]
    async fn a_pre_migration_row_restores_with_zero_totals() {
        let row: ConversationData = serde_json::from_value(serde_json::json!({
            "id": "old-1",
            "title": "Old",
            "model_id": "m1",
            "message_history": "[]",
            "system_traces": "[]",
            "created_at": 1,
            "updated_at": 2,
        }))
        .expect("an old row still loads");
        let mut session = AgentSession::new(config());
        session
            .restore_conversation(
                row,
                &unpriced_model(),
                &ollama_provider(),
                AgentBuildContext::from_services(AgentServices::default()),
            )
            .await
            .expect("an old conversation opens without error");
        let conversation = session.conversation().unwrap();
        assert_eq!(conversation.tool_call_count(), 0);
        assert_eq!(conversation.context_tokens(), 0);
    }
}

// ── The turn's notification wiring (AGE-362) ─────────────────────────────────

/// AGE-346 gave `WriteApprovalStore` a resolution notifier and `prepare_turn`
/// the line that installs it. These cover the line, not just the store: delete
/// `self.write_approvals.set_notifiers(..)` and the first two fail. Without
/// them that deletion is silent, which is exactly how the original bug shipped.
///
/// Ids come off the turn's own `approval_rx` rather than out of the stores'
/// pending maps, so each test also says something true about the request side:
/// a parked prompt is announced to the turn that is running.
mod turn_notification_wiring {
    use super::*;
    use crate::models::execution_approval_store::{ApprovalDecision, request_execution_approval};
    use crate::models::write_approval_store::{WriteApprovalDecision, WriteOperation};
    use crate::settings::models::execution_settings::ApprovalMode;
    use crate::tools::filesystem_write_tool::request_write_approval;

    /// Park a real write-approval request, and return the id the turn was told
    /// about. Going through `request_write_approval` keeps this honest about
    /// how a tool actually asks.
    async fn park_write_approval(session: &AgentSession, turn: &mut PreparedTurn) -> String {
        let pending = session.write_approvals().get_pending_approvals();
        tokio::spawn(async move {
            let _ = request_write_approval(
                &pending,
                &ApprovalMode::AlwaysAsk,
                WriteOperation::DeleteFile {
                    path: "/tmp/x".to_string(),
                },
            )
            .await;
        });
        next_request(turn, "write").await
    }

    async fn park_execution_approval(session: &AgentSession, turn: &mut PreparedTurn) -> String {
        let pending = session.execution_approvals().get_pending_approvals();
        tokio::spawn(async move {
            let _ = request_execution_approval(
                &pending,
                &ApprovalMode::AlwaysAsk,
                "rm -rf /tmp/x",
                false,
            )
            .await;
        });
        next_request(turn, "execution").await
    }

    /// Every wait in here is bounded. A regression in the wiring means nothing
    /// is ever sent, and an unbounded `recv().await` would turn that into a
    /// hung suite rather than a failing test -- which is worse than no test,
    /// because CI reports it as a timeout with nothing to read.
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

    async fn next_resolution(
        turn: &mut PreparedTurn,
        what: &str,
    ) -> crate::models::execution_approval_store::ApprovalResolution {
        tokio::time::timeout(BUDGET, turn.resolution_rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}: the turn was never told"))
            .unwrap_or_else(|| panic!("the resolution channel closed before {what}"))
    }

    async fn next_request(turn: &mut PreparedTurn, what: &str) -> String {
        tokio::time::timeout(BUDGET, turn.approval_rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for the {what} prompt to be announced"))
            .expect("the approval channel stays open for the turn")
            .id
    }

    fn prepare(session: &mut AgentSession, text: &str) -> PreparedTurn {
        session
            .prepare_turn(TurnInput::text(text), Arc::new(AtomicBool::new(false)))
            .expect("the turn prepares")
    }

    #[tokio::test]
    async fn a_resolved_write_approval_reaches_the_turn_that_is_running() {
        for (decision, expected) in [
            (WriteApprovalDecision::Approved, true),
            (WriteApprovalDecision::Denied, false),
        ] {
            let mut session = session_with_conversation().await;
            let mut turn = prepare(&mut session, "edit the file");

            let id = park_write_approval(&session, &mut turn).await;
            assert!(session.write_approvals().resolve(&id, decision));

            let resolution = next_resolution(&mut turn, "the write approval's answer").await;
            assert_eq!(resolution.id, id);
            assert_eq!(resolution.approved, expected);
        }
    }

    /// One channel, both stores. The turn holds a single `resolution_rx` and
    /// the client cannot tell which store answered — nor should it have to.
    #[tokio::test]
    async fn both_approval_stores_report_to_the_same_receiver() {
        let mut session = session_with_conversation().await;
        let mut turn = prepare(&mut session, "do both");

        let write_id = park_write_approval(&session, &mut turn).await;
        assert!(
            session
                .write_approvals()
                .resolve(&write_id, WriteApprovalDecision::Approved)
        );
        let first = next_resolution(&mut turn, "the write answer").await;
        assert_eq!(first.id, write_id);

        // The execution store has always reported; this asserts the two have
        // not drifted onto separate channels.
        let exec_id = park_execution_approval(&session, &mut turn).await;
        assert!(
            session
                .execution_approvals()
                .resolve(&exec_id, ApprovalDecision::Denied)
        );
        let second = next_resolution(&mut turn, "the execution answer").await;
        assert_eq!(second.id, exec_id);
        assert!(!second.approved);
    }

    /// AGE-246 / D7: the channels are per turn. An answer given during the
    /// second turn must not reach the first turn's receiver. The execution
    /// store has this property covered; the write store never did.
    #[tokio::test]
    async fn a_new_turn_replaces_the_previous_turns_receiver() {
        let mut session = session_with_conversation().await;
        let mut first = prepare(&mut session, "one");

        // `finish_turn` is what ends a turn -- it clears the cancel flag that
        // `is_turn_active` reads, which `cancel()` deliberately does not.
        // The first turn is never driven here; only its channels matter.
        session.finish_turn(None, Vec::new());
        let mut second = prepare(&mut session, "two");

        let id = park_write_approval(&session, &mut second).await;
        assert!(
            session
                .write_approvals()
                .resolve(&id, WriteApprovalDecision::Approved)
        );

        let resolution = next_resolution(&mut second, "the running turn's answer").await;
        assert_eq!(resolution.id, id);
        assert!(
            first.resolution_rx.try_recv().is_err(),
            "the previous turn's receiver must not see the new turn's answer"
        );
    }
}
