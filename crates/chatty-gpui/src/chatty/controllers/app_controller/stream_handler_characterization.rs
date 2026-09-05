//! Characterization of the desktop send path (AGE-191).
//!
//! Drives the real [`GpuiStreamHandler`] through chatty-core's `run_stream_loop`
//! against the scripted scenarios in `chatty_core::services::stream_fixtures`,
//! and records the resulting [`StreamManagerEvent`] sequence as a golden file.
//!
//! This is the desktop counterpart to `chatty-tui`'s
//! `engine/streaming_characterization.rs`, and both script from the same
//! fixtures: a divergence between the two frontends shows up as a difference
//! between two golden directories rather than as behaviour nobody notices.
//!
//! The handler talks to real entities — a `ChatView` in a headless test window,
//! a live `StreamManager` — so what is recorded is what the UI would actually
//! receive. The controller handle is deliberately invalid: `run_llm_stream`
//! holds a `WeakEntity<ChattyApp>` that can already be gone by the time a
//! background turn finishes, and every use of it in the handler is best-effort.
//!
//! # What these goldens cover, and what they do not
//!
//! Covered: the `StreamManagerEvent` stream, which is what the transcript UI
//! subscribes to — every chunk's mapping, text batching, the API turn counter,
//! the terminal `StreamEnded` and the follow-up the handler queues.
//!
//! Not covered:
//!
//! * **Sub-agent progress.** The desktop routes it to `ConversationsStore` and
//!   `ChatView` rather than through `StreamManager`, so none of it appears
//!   here — `sub_agent_progress.txt` records only that scenario's chunks. The
//!   store holds no conversation under the test's id, so those writes are
//!   no-ops. Catching a regression in `on_progress` needs a real `Conversation`
//!   in the store, which needs a model and provider config; worth doing, not
//!   done here.
//! * **User-pressed Stop.** `cancelled_mid_stream` sets the cancel flag
//!   directly, which is the loop-guard and todo-protocol path. A user Stop goes
//!   through `StreamManager::stop_stream`, which reports the turn differently —
//!   that flag-only path is why this golden ends `Completed` rather than
//!   `Cancelled`.
//!
//! # A divergence these pin
//!
//! `provider_error_mid_stream` ends differently in the two frontends: the
//! desktop converts a transport `Err` into a `StreamChunk::Error`, emits
//! `StreamEnded(Error)` and returns `Ok` from the loop, while the TUI lets the
//! `?` propagate and the loop returns `Err` with no terminal event at all.
//! Recorded as-is; reconciling them is a later phase's decision.

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
            ..
        } => format!("TokenUsage(in={input_tokens}, out={output_tokens})"),
        StreamManagerEvent::StreamEnded {
            status,
            token_usage,
            api_turn_count,
            ..
        } => {
            // The epoch is a monotonic counter shared across the process, so it
            // is not stable across a run of several scenarios and is left out.
            format!(
                "StreamEnded(status={status:?}, usage={token_usage:?}, api_turns={api_turn_count})"
            )
        }
    }
}

/// Run one scenario through the real handler and return the events the UI saw.
async fn record(scenario: Scenario, cx: &mut gpui::TestAppContext) -> Vec<String> {
    let conv_id = "characterization-conv".to_string();
    let cancel_flag = Arc::new(AtomicBool::new(false));

    cx.update(|cx| {
        gpui_component::init(cx);
        if !cx.has_global::<ConversationsStore>() {
            cx.set_global(ConversationsStore::new());
        }
        if !cx.has_global::<ExecutionSettingsModel>() {
            cx.set_global(ExecutionSettingsModel::default());
        }
    });

    // A real ChatView, in a headless window, under a `gpui_component::Root` —
    // the component library asserts the window's first layer is one, and the
    // app builds its window the same way. `cx.entity()` is not usable here for
    // the same reason: the root is the Root, not the view.
    let chat_view_slot: Rc<RefCell<Option<gpui::Entity<ChatView>>>> = Rc::default();
    let slot = chat_view_slot.clone();
    let _window = cx.add_window(move |window, cx| {
        let view = cx.new(|cx| ChatView::new(window, cx));
        *slot.borrow_mut() = Some(view.clone());
        gpui_component::Root::new(view, window, cx)
    });
    let chat_view = chat_view_slot
        .borrow()
        .clone()
        .expect("the window builder ran and stored its entity");

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

    // Register the turn, so the manager has the state `handle_chunk` mutates
    // (text batching, the API turn counter) rather than dropping chunks.
    cx.update(|cx| {
        stream_manager.update(cx, |manager: &mut StreamManager, cx| {
            let task = cx.background_executor().spawn(async { Ok(()) });
            manager.register_stream(conv_id.clone(), task, cancel_flag.clone(), None, cx);
        });
    });

    // Progress is queued before the loop starts rather than interleaved with
    // chunks — see the fixture module on why the interleave is not contract.
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
    for progress in scenario.progress {
        progress_tx
            .send(progress)
            .expect("receiver is alive for the whole scenario");
    }
    drop(progress_tx);

    let mut stream = scripted_stream(scenario.items, cancel_flag.clone());
    let mut handler = GpuiStreamHandler {
        conv_id: conv_id.clone(),
        cx: cx.to_async(),
        chat_view,
        stream_manager: Some(stream_manager.clone()),
        weak_ctrl: gpui::WeakEntity::new_invalid(),
        // The plainest provider: no Azure refresh branch, no OpenRouter auth branch,
        // so a `StreamChunk::Error` takes the ordinary path.
        provider_type: chatty_core::settings::models::providers_store::ProviderType::Ollama,
        agent_task_controller: chatty_core::services::AgentTaskController::new(),
        loop_guard: chatty_core::services::AgentLoopGuard::new(10, false),
        cancel_flag: cancel_flag.clone(),
        pending_tool_name: std::collections::HashMap::new(),
        pending_tool_args: std::collections::HashMap::new(),
        pending_follow_up: None,
        stream_errored: false,
        text_overflow_stop_requested: false,
    };

    let outcome = run_stream_loop(&mut stream, &mut progress_rx, &cancel_flag, &mut handler).await;

    // `run_llm_stream`'s finalization, minus the trace read: a turn that did not
    // end in error is finalized by the caller, and that is where the UI's
    // StreamEnded comes from for the completed cases.
    if !handler.stream_errored {
        cx.update(|cx| {
            stream_manager.update(cx, |manager: &mut StreamManager, cx| {
                manager.finalize_stream(&conv_id, cx)
            });
        });
    }

    // `cx.emit` is delivered on the next effect flush, not inline.
    cx.run_until_parked();
    drop(subscription);

    let mut recorded = events.borrow().clone();
    recorded.push(match outcome {
        Ok(()) => "=> loop returned Ok".to_string(),
        Err(e) => format!("=> loop returned Err({:?})", e.to_string()),
    });
    recorded.push(match handler.pending_follow_up {
        // The prompt text is long and tuned often; the golden pins that a
        // follow-up was queued and which protocol asked for it.
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
