//! Characterization of the TUI send path (AGE-191, reparented in AGE-195).
//!
//! The TUI's turn is chatty-core's `AgentSession`; what the engine sees is
//! its `SessionEvent` stream through `From<SessionEvent> for AppEvent`. This
//! replays the scripted scenarios in `chatty_core::services::stream_fixtures`
//! through exactly that path and records the resulting [`AppEvent`] sequence
//! as a golden file — the contract a change to the turn must not alter by
//! accident. A deliberate change is made by re-running with
//! `UPDATE_GOLDENS=1` and explaining the diff in review.
//!
//! The matching desktop goldens live in
//! `crates/chatty-gpui/src/chatty/controllers/app_controller/goldens/`.

use std::path::PathBuf;

use chatty_core::services::{
    Scenario, StreamSurface, assert_golden, clarification_scenario, scenarios,
};
use chatty_core::session::{TurnPolicy, replay_scenario};

use crate::events::AppEvent;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine/goldens")
}

/// The interactive TUI's policy, as `ChatEngine::new` configures its session.
fn policy() -> TurnPolicy {
    TurnPolicy {
        surface: StreamSurface::InteractiveTui,
        max_agent_turns: 10,
        loop_guard: false,
        already_asked_to_retry: false,
    }
}

/// One line per event, in a shape that reads as a diff.
///
/// `AppEvent`'s `Debug` is for logs; spelling the lines out here keeps the
/// golden stable when an unrelated variant or field is added.
fn describe(event: &AppEvent) -> String {
    match event {
        AppEvent::StreamStarted => "StreamStarted".to_string(),
        AppEvent::TextChunk(text) => format!("TextChunk({text:?})"),
        AppEvent::ToolCallStarted { id, name } => format!("ToolCallStarted(id={id:?}, {name:?})"),
        AppEvent::ToolCallInput { id, arguments } => {
            format!("ToolCallInput(id={id:?}, {arguments:?})")
        }
        AppEvent::ToolCallResult { id, result } => format!("ToolCallResult(id={id:?}, {result:?})"),
        AppEvent::ToolCallError { id, error } => format!("ToolCallError(id={id:?}, {error:?})"),
        AppEvent::ApprovalRequested {
            id,
            command,
            is_sandboxed,
        } => format!("ApprovalRequested(id={id:?}, {command:?}, sandboxed={is_sandboxed})"),
        AppEvent::ApprovalResolved { id, approved } => {
            format!("ApprovalResolved(id={id:?}, approved={approved})")
        }
        AppEvent::ClarificationRequested { id, questions } => {
            let texts: Vec<&str> = questions.iter().map(|q| q.question.as_str()).collect();
            format!("ClarificationRequested(id={id:?}, {texts:?})")
        }
        AppEvent::ApiCallUsage(call) => format!(
            "ApiCallUsage(turn={}, in={}, out={}, cache_read={}, cache_write={})",
            call.turn,
            call.input_tokens,
            call.output_tokens,
            call.cache_read_tokens,
            call.cache_write_tokens
        ),
        AppEvent::TokenUsage(usage) => format!(
            "TokenUsage(in={}, out={}, cache_read={}, cache_write={})",
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens
        ),
        AppEvent::StreamCompleted => "StreamCompleted".to_string(),
        AppEvent::StreamCancelled => "StreamCancelled".to_string(),
        AppEvent::StreamError(error) => format!("StreamError(kind={:?})", error.kind),
        AppEvent::AgentProtocolFollowUp(prompt) => {
            // The prompt text is long and tuned often; the golden pins that a
            // follow-up was injected and which protocol asked for it, not its
            // exact wording.
            let kind = if prompt.contains("write_todos") {
                "write_todos"
            } else if prompt.contains("verify_completion") {
                "verify_completion"
            } else {
                "other"
            };
            format!("AgentProtocolFollowUp({kind})")
        }
        AppEvent::SubAgentProgress(text) => format!("SubAgentProgress({text:?})"),
        AppEvent::SubAgentFinished(text) => format!("SubAgentFinished({text:?})"),
        // Rendered as the transcript line it becomes, so the goldens read
        // the same whether the progress arrived typed or as a line.
        AppEvent::SubAgent(progress) => {
            let line = super::helpers::sub_agent_line(progress);
            if matches!(
                progress,
                chatty_core::tools::invoke_agent_tool::InvokeAgentProgress::Finished { .. }
            ) {
                format!("SubAgentFinished({line:?})")
            } else {
                format!("SubAgentProgress({line:?})")
            }
        }
        // Lifecycle and terminal events, which a turn never produces.
        // Recorded rather than ignored so a turn that starts emitting one is
        // caught instead of quietly passing.
        _ => "UNEXPECTED(non-stream event)".to_string(),
    }
}

/// Run one scenario through the session and the adapter, and return the
/// events the engine would handle.
async fn record(scenario: Scenario) -> Vec<String> {
    replay_scenario(scenario, policy())
        .await
        .into_iter()
        .map(AppEvent::from)
        .map(|event| describe(&event))
        .collect()
}

#[tokio::test]
async fn tui_send_path_matches_goldens() {
    let dir = goldens_dir();
    for scenario in scenarios().into_iter().chain([clarification_scenario()]) {
        let name = scenario.name;
        let events = record(scenario).await;
        assert_golden(&dir, name, &events);
    }
}

/// The goldens are only a safety net if a changed ordering actually fails.
///
/// Compares against the committed file directly rather than through
/// [`assert_golden`], which rewrites instead of comparing under
/// `UPDATE_GOLDENS` — the one run where this test would otherwise pass
/// vacuously.
#[tokio::test]
async fn a_changed_ordering_is_detectable() {
    let mut events = record(
        scenarios()
            .into_iter()
            .find(|s| s.name == "tool_call_then_result")
            .expect("scenario exists"),
    )
    .await;

    let golden = std::fs::read_to_string(goldens_dir().join("tool_call_then_result.txt"))
        .expect("golden is committed");
    assert_eq!(
        golden,
        format!("{}\n", events.join("\n")),
        "the unmodified recording must match its golden"
    );

    events.swap(0, 1);
    assert_ne!(
        golden,
        format!("{}\n", events.join("\n")),
        "swapping two events must change the recorded sequence"
    );
}
