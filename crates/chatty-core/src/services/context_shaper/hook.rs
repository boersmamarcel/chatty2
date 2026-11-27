//! The rig `AgentHook` of the guard, and the mid-run messages (TM-5) it
//! delivers over the worker's broker connection.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use chatty_fabric::{CallEvent, CallRequest, CallResult, Transport};
use futures::StreamExt;
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, OutcomeAction, OutcomeEvent,
    RequestPatch,
};
use rig_core::completion::Message;
use rig_core::completion::message::ToolResultContent;
use rig_core::tool::ToolOutput;
use tracing::{debug, warn};

use super::ContextShaper;
use super::budget::{MIN_CLAMPED_REPLY_TOKENS, ReplyRoom, WindowClamped};

impl ContextShaper {
    /// Take this worker's mid-run messages (TM-5) over `transport`, its
    /// broker connection, at every tool result it records. Set once.
    pub fn deliver_mid_run_messages(&self, transport: Arc<dyn Transport>) {
        let _ = self.inner.mid_run.set(transport);
    }

    /// The messages waiting on this worker's mid-run list, wrapped by the
    /// broker; none without a connection. A failed take delivers nothing
    /// now and leaves the messages for the next tool round or run.
    pub(super) async fn take_mid_run_messages(&self) -> Vec<String> {
        let Some(transport) = self.inner.mid_run.get() else {
            return Vec::new();
        };
        let mut stream = match transport.call(CallRequest::TakeMessages).await {
            Ok(stream) => stream,
            Err(error) => {
                warn!(%error, "mid-run messages: mailbox.take failed");
                return Vec::new();
            }
        };
        while let Some(event) = stream.next().await {
            match event {
                Ok(CallEvent::Result(CallResult::Messages(messages))) => return messages,
                Ok(_) => continue,
                Err(error) => {
                    warn!(%error, "mid-run messages: mailbox.take failed");
                    return Vec::new();
                }
            }
        }
        Vec::new()
    }
}

impl AgentHook for ContextShaper {
    async fn on_completion_call(
        &self,
        ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        // The results recorded after this call are the next round's.
        self.start_round();
        let extra_reserve = if ctx.scratchpad().contains::<ReplyRoom>() {
            self.reply_room_reserve()
        } else {
            0
        };
        // rig 0.46 shares each message of the run's history; the shaper reads
        // and rewrites owned ones.
        let history: Vec<Message> = event.history.iter().map(|m| (**m).clone()).collect();
        let shaped = self
            .prepare_request_reserving(&history, event.prompt, event.turn, extra_reserve)
            .await;
        let sent = shaped.as_deref().unwrap_or(&history);
        if let Some(reason) = self.window_overflow(sent, event.prompt) {
            warn!(turn = event.turn, %reason, "context shaper: request over the window; not sent");
            return CompletionCallAction::stop(reason);
        }
        let cap = self.clamped_output_cap(sent, event.prompt);
        let exhausted = self
            .reply_room_left(sent, event.prompt)
            .is_some_and(|room| room < MIN_CLAMPED_REPLY_TOKENS);
        self.inner
            .window_exhausted
            .store(exhausted, Ordering::Relaxed);
        let mut patch = RequestPatch::new();
        if let Some(cap) = cap {
            debug!(
                turn = event.turn,
                max_tokens = cap,
                "context shaper: output cap clamped to the room the window has left"
            );
            ctx.scratchpad().insert(WindowClamped);
            patch = patch.max_tokens(cap);
        }
        if let Some(history) = shaped {
            patch = patch.history(history);
        }
        if patch == RequestPatch::new() {
            CompletionCallAction::Continue
        } else {
            CompletionCallAction::patch(patch)
        }
    }

    async fn on_outcome(&self, _ctx: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        let Some(result) = event.tool_result() else {
            return OutcomeAction::Proceed;
        };
        let tool_name = event.tool_name().unwrap_or_default();
        // The provider refused an image earlier in this run (AGE-2178): an
        // image a tool returns now would only be refused again.
        let without_images = self
            .inner
            .images_refused
            .load(Ordering::Relaxed)
            .then(|| without_images(result.output()))
            .flatten();
        let recorded = self
            .record_round_output(without_images.as_ref().unwrap_or(result.output()))
            .or(without_images);
        if recorded.is_some() {
            warn!(
                tool = tool_name,
                cap_tokens = self.recording_cap(),
                "context shaper: tool result larger than the recording cap; recorded truncated"
            );
        }
        let messages = self.take_mid_run_messages().await;
        if messages.is_empty() {
            return recorded.map_or(OutcomeAction::Proceed, |output| {
                OutcomeAction::rewrite_tool_output(&event, output)
            });
        }
        debug!(
            tool = tool_name,
            messages = messages.len(),
            "Delivering mid-run messages with a tool result"
        );
        OutcomeAction::rewrite_tool_output(
            &event,
            with_messages(
                recorded.unwrap_or_else(|| result.output().clone()),
                &messages,
            ),
        )
    }
}

/// `output` with each image replaced by the note a text-only model gets
/// (AGE-2178); `None` when it has none.
fn without_images(output: &ToolOutput) -> Option<ToolOutput> {
    let content = output.clone().into_content();
    if !content
        .iter()
        .any(|c| matches!(c, ToolResultContent::Image(_)))
    {
        return None;
    }
    let content = content
        .into_iter()
        .map(|c| match c {
            ToolResultContent::Image(_) => {
                ToolResultContent::text(crate::services::llm_service::IMAGE_UNSUPPORTED_NOTE)
            }
            other => other,
        })
        .collect();
    ToolOutput::content(content).ok()
}

/// `output` with `messages` (already wrapped) after it, as one more text
/// block of the same tool result.
fn with_messages(output: ToolOutput, messages: &[String]) -> ToolOutput {
    let mut content = output.into_content();
    content.push(ToolResultContent::text(messages.join("\n")));
    ToolOutput::content(content).expect("a tool result with a text block is valid")
}
