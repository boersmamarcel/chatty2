//! The last model request of a run, kept for when the run fails.
//!
//! rig hands a run's messages back only with its final response (or with a
//! max-turns / cancelled error). Any other error — a transport failure, a
//! 5xx, a malformed tool call — ends the run with none of them, so every
//! tool round-trip the run made was lost and a retry went out on a history
//! that no longer held the work it was retrying. This hook records what
//! every model call is about to send; when the run fails, `stream_prompt`
//! hands that record on as the turn's messages, so the round-trips are
//! persisted like a finished turn's and the retry runs on them.

use std::sync::Arc;

use parking_lot::Mutex;
use rig_agent::agent::{AgentHook, CompletionCallAction, CompletionCallEvent, HookContext};
use rig_core::completion::Message;

/// Records the messages of each model call (history, then the prompt) as it
/// goes out. One agent runs one turn at a time, so one slot is enough.
#[derive(Clone, Debug, Default)]
pub struct RequestRecorder {
    last: Arc<Mutex<Option<Vec<Message>>>>,
}

impl RequestRecorder {
    /// Forget the previous run's request; called when a run starts.
    pub fn clear(&self) {
        *self.last.lock() = None;
    }

    /// Keep `messages` as the request that just went out.
    pub fn record(&self, messages: Vec<Message>) {
        *self.last.lock() = Some(messages);
    }

    /// The messages of the last model call of the run, if one went out.
    pub fn take(&self) -> Option<Vec<Message>> {
        self.last.lock().take()
    }
}

impl AgentHook for RequestRecorder {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        // rig's own history, not what the context shaper patched it to:
        // what is persisted is the run, not one request's view of it.
        let mut messages = Vec::with_capacity(event.history.len() + 1);
        messages.extend_from_slice(event.history);
        messages.push(event.prompt.clone());
        self.record(messages);
        CompletionCallAction::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use rig_agent::agent::AgentBuilder;
    use rig_agent::streaming::StreamingPrompt;
    use rig_agent::test_utils::MockAddTool;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    /// A run whose second model call fails: the record holds the prompt,
    /// the first call's tool call and its result — the request that failed.
    #[tokio::test]
    async fn a_failed_call_leaves_the_request_it_was_sending() {
        let model = MockCompletionModel::from_stream_turns([
            vec![
                MockStreamEvent::tool_call(
                    "tool_call_1",
                    "add",
                    serde_json::json!({"x": 1, "y": 2}),
                )
                .with_call_id("call_1"),
                MockStreamEvent::final_response_with_total_tokens(4),
            ],
            vec![MockStreamEvent::error("error sending request for url")],
        ]);
        let recorder = RequestRecorder::default();
        let agent = AgentBuilder::new(model)
            .tool(MockAddTool)
            .add_hook(recorder.clone())
            .build();

        let mut stream = agent.stream_prompt("add one and two").max_turns(3).await;
        let mut failed = false;
        while let Some(item) = stream.next().await {
            failed |= item.is_err();
        }
        assert!(failed, "the second call's error ends the run");

        let recorded = recorder.take().expect("two calls went out");
        assert_eq!(recorded.len(), 3, "{recorded:?}");
        assert!(format!("{:?}", recorded[0]).contains("add one and two"));
        assert!(matches!(recorded[1], Message::Assistant { .. }));
        assert!(format!("{:?}", recorded[2]).contains("ToolResult"));
        assert!(recorder.take().is_none(), "take empties the slot");
    }
}
