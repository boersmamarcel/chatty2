//! Characterization of the desktop send path (AGE-191, reparented in AGE-195).
//!
//! The desktop's turn is chatty-core's `AgentSession`; what the UI sees is
//! its `SessionEvent` stream through `StreamManager::handle_session_event`.
//! This replays the scripted scenarios in
//! `chatty_core::services::stream_fixtures` through exactly that path and
//! records the resulting [`StreamManagerEvent`] sequence as a golden file —
//! the contract a change to the turn must not alter by accident. A
//! deliberate change is made by re-running with `UPDATE_GOLDENS=1` and
//! explaining the diff in review.
//!
//! The matching TUI goldens live in `crates/chatty-tui/src/engine/goldens/`,
//! and both frontends script from the same fixtures.
//!
//! # What these goldens cover, and what they do not
//!
//! Covered: the `StreamManagerEvent` stream, which is what the transcript UI
//! subscribes to — every event's mapping, text batching, the terminal
//! `StreamEnded` and the follow-up the session queues.
//!
//! Not covered:
//!
//! * **Sub-agent progress.** The desktop routes it to `ConversationsStore` and
//!   `ChatView` rather than through `StreamManager`, so none of it appears
//!   here — `sub_agent_progress.txt` records only that scenario's chunks.
//! * **User-pressed Stop.** `cancelled_mid_stream` sets the cancel flag
//!   directly, which is the loop-guard and todo-protocol path. A user Stop
//!   goes through `StreamManager::stop_stream`, which reports the turn
//!   differently — that flag-only path is why this golden ends `Completed`
//!   rather than `Cancelled`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use chatty_core::services::{
    Scenario, assert_golden, clarification_scenario, run_stream_loop, scenarios, scripted_stream,
};

// Brings `ChatView`, `ConversationsStore`, `ExecutionSettingsModel` and gpui's
// `AppContext` into scope, the same way `message_ops_internals` gets them.
use super::*;

// `use super::*` pulls in `gpui::test`, which shadows the standard `#[test]`
// that `#[gpui::test]` expands into — expanding it forever. Re-import the real
// one, as the sibling test modules in this crate do.
use crate::chatty::models::{StreamManager, StreamManagerEvent};
#[allow(unused_imports)]
use core::prelude::rust_2021::test;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/chatty/controllers/app_controller/goldens")
}

/// One line per event. `conversation_id` is constant across a scenario and is
/// left out; everything a subscriber branches on is kept.
fn describe(event: &StreamManagerEvent) -> String {
    match event {
        StreamManagerEvent::StreamStarted { .. } => "StreamStarted".to_string(),
        StreamManagerEvent::TextChunk { text, .. } => format!("TextChunk({text:?})"),
        StreamManagerEvent::ToolCallStarted { id, name, .. } => {
            format!("ToolCallStarted(id={id:?}, {name:?})")
        }
        StreamManagerEvent::ToolCallInput { id, arguments, .. } => {
            format!("ToolCallInput(id={id:?}, {arguments:?})")
        }
        StreamManagerEvent::ToolCallResult { id, result, .. } => {
            format!("ToolCallResult(id={id:?}, {result:?})")
        }
        StreamManagerEvent::ToolCallError { id, error, .. } => {
            format!("ToolCallError(id={id:?}, {error:?})")
        }
        StreamManagerEvent::ApprovalRequested {
            id,
            command,
            is_sandboxed,
            ..
        } => format!("ApprovalRequested(id={id:?}, {command:?}, sandboxed={is_sandboxed})"),
        StreamManagerEvent::ApprovalResolved { id, approved, .. } => {
            format!("ApprovalResolved(id={id:?}, approved={approved})")
        }
        StreamManagerEvent::ClarificationRequested { id, questions, .. } => {
            let texts: Vec<&str> = questions.iter().map(|q| q.question.as_str()).collect();
            format!("ClarificationRequested(id={id:?}, {texts:?})")
        }
        StreamManagerEvent::TokenUsage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            ..
        } => format!(
            "TokenUsage(in={input_tokens}, out={output_tokens}, cache_read={cache_read_tokens}, cache_write={cache_write_tokens})"
        ),
        StreamManagerEvent::StreamEnded {
            status,
            token_usage,
            ..
        } => {
            // The epoch is a monotonic counter shared across the process, so it
            // is not stable across a run of several scenarios and is left out.
            let usage = token_usage.as_ref().map(|u| {
                format!(
                    "in={}, out={}, cache_read={}, cache_write={}, api_turns={}, calls={}",
                    u.input_tokens,
                    u.output_tokens,
                    u.cache_read_tokens,
                    u.cache_write_tokens,
                    u.api_turn_count,
                    u.calls.len()
                )
            });
            format!("StreamEnded(status={status:?}, usage={usage:?})")
        }
    }
}

/// Run one scenario through chatty-core's session handler and
/// `StreamManager::handle_session_event`, recording what the UI would see.
///
/// This is `run_llm_stream`'s path: the manager keeps the lifecycle
/// (registration, text batching, the terminal `StreamEnded`), and the
/// turn's own logic — the follow-up, the usage folding, the loop guard — is
/// the session's.
async fn record(scenario: Scenario, cx: &mut gpui::TestAppContext) -> Vec<String> {
    let conv_id = "characterization-conv".to_string();
    let stream_manager = cx.update(|cx| cx.new(|_cx| StreamManager::new()));

    let events: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = events.clone();
    let subscription = cx.update(|cx| {
        cx.subscribe(
            &stream_manager,
            move |_manager, event: &StreamManagerEvent, _cx| {
                sink.borrow_mut().push(describe(event));
            },
        )
    });

    let cancel_flag = Arc::new(AtomicBool::new(false));
    cx.update(|cx| {
        stream_manager.update(cx, |manager: &mut StreamManager, cx| {
            let task = cx.background_executor().spawn(async { Ok(()) });
            manager.register_stream(conv_id.clone(), task, cancel_flag.clone(), None, cx);
        });
    });

    // The desktop's policy (`desktop_session_config`): the loop guard runs.
    let session_events = chatty_core::session::replay_scenario(
        scenario,
        chatty_core::session::TurnPolicy {
            surface: chatty_core::services::StreamSurface::Desktop,
            max_agent_turns: 10,
            loop_guard: true,
            already_asked_to_retry: false,
        },
    )
    .await;

    let mut follow_up = None;
    cx.update(|cx| {
        stream_manager.update(cx, |manager: &mut StreamManager, cx| {
            for event in session_events {
                if let chatty_core::session::SessionEvent::FollowUp(prompt) = &event {
                    follow_up = Some(prompt.clone());
                }
                manager.handle_session_event(&conv_id, event, cx);
            }
        });
    });

    cx.run_until_parked();
    drop(subscription);

    let mut recorded = events.borrow().clone();
    // The session never propagates an error out of the loop; the trailer is
    // kept so the recording stays byte-comparable with the Phase 0 golden.
    recorded.push("=> loop returned Ok".to_string());
    recorded.push(match follow_up {
        Some(ref prompt) if prompt.contains("write_todos") => {
            "=> follow-up queued: write_todos".to_string()
        }
        Some(ref prompt) if prompt.contains("verify_completion") => {
            "=> follow-up queued: verify_completion".to_string()
        }
        Some(_) => "=> follow-up queued: other".to_string(),
        None => "=> no follow-up".to_string(),
    });
    recorded
}

#[gpui::test]
async fn desktop_send_path_matches_goldens(cx: &mut gpui::TestAppContext) {
    // The shared loop's idle tick is a `tokio::time::sleep`. The desktop enters
    // a Tokio runtime for the whole app lifetime in `main` and lets GPUI's
    // executor poll the futures; without the same runtime in scope here, the
    // sleep panics for want of a timer. Entering one mirrors production rather
    // than working around it.
    let runtime = tokio::runtime::Runtime::new().expect("failed to create a Tokio runtime");
    let _guard = runtime.enter();

    let dir = goldens_dir();
    for scenario in scenarios().into_iter().chain([clarification_scenario()]) {
        let name = scenario.name;
        let events = record(scenario, cx).await;
        assert_golden(&dir, name, &events);
    }
}
