//! Characterization of the TUI send path (AGE-191).
//!
//! Drives [`TuiStreamHandler`] through the shared `run_stream_loop` against the
//! scripted scenarios in `chatty_core::services::stream_fixtures`, and records
//! the resulting [`AppEvent`] sequence as a golden file.
//!
//! These goldens are the contract for AGE-190's phases 1–4: the desktop adopting
//! the core loop, and both frontends later reparenting onto `AgentSession`, must
//! leave every sequence here byte-identical. A deliberate change is made by
//! re-running with `UPDATE_GOLDENS=1` and explaining the diff in review.
//!
//! The matching desktop goldens live in
//! `crates/chatty-gpui/src/chatty/controllers/app_controller/goldens/`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chatty_core::services::{
    AgentTaskController, Scenario, assert_golden, clarification_scenario, run_stream_loop,
    scenarios, scripted_stream,
};
use tokio::sync::mpsc;

use super::TuiStreamHandler;
use crate::events::AppEvent;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/engine/goldens")
}

/// One line per event, in a shape that reads as a diff.
///
/// `AppEvent` has no `Debug`, and deriving one just for tests would put a
/// formatting choice in production code. Spelling it out here also keeps the
/// golden stable when an unrelated variant is added.
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
        AppEvent::TokenUsage {
            input_tokens,
            output_tokens,
        } => format!("TokenUsage(in={input_tokens}, out={output_tokens})"),
        AppEvent::StreamCompleted => "StreamCompleted".to_string(),
        AppEvent::StreamCancelled => "StreamCancelled".to_string(),
        AppEvent::StreamError(message) => format!("StreamError({message:?})"),
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
        // Lifecycle and terminal events, which the stream loop never sends.
        // Recorded rather than ignored so a loop that starts emitting one is
        // caught instead of quietly passing.
        _ => "UNEXPECTED(non-stream event)".to_string(),
    }
}

/// Run one scenario through the real loop and return the events it produced.
async fn record(scenario: Scenario) -> Vec<String> {
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Progress is queued before the loop starts rather than interleaved with
    // chunks — see the fixture module on why the interleave is not contract.
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
    for progress in scenario.progress {
        progress_tx
            .send(progress)
            .expect("receiver is alive for the whole scenario");
    }
    drop(progress_tx);

    let mut stream = scripted_stream(scenario.items, cancel_flag.clone());
    let mut handler = TuiStreamHandler {
        event_tx,
        task_controller: AgentTaskController::new(),
        pending_tool_names: HashMap::new(),
        pending_follow_up: None,
        cancelled: false,
    };

    // A transport `Err` propagates out of `on_chunk` and so out of the loop —
    // `run_stream` hands it to the caller rather than turning it into an event.
    // That is part of the contract, so the outcome is recorded, not unwrapped.
    let outcome = run_stream_loop(&mut stream, &mut progress_rx, &cancel_flag, &mut handler).await;

    let mut events = Vec::new();
    while let Ok(event) = event_rx.try_recv() {
        events.push(describe(&event));
    }
    events.push(match outcome {
        Ok(()) => "=> loop returned Ok".to_string(),
        Err(e) => format!("=> loop returned Err({:?})", e.to_string()),
    });
    events
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
