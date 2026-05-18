//! Stream processing: maps LLM stream chunks to TUI `AppEvent`s.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use chatty_core::factories::AgentClient;
use chatty_core::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};
use chatty_core::services::{AgentTaskController, ChunkAction, StreamChunk, stream_prompt};
use chatty_core::tools::invoke_agent_tool::{InvokeAgentProgress, InvokeAgentProgressSlot};
use rig_core::message::UserContent;
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::events::AppEvent;

pub(super) struct StreamParams {
    pub agent: AgentClient,
    pub history: Vec<rig_core::completion::Message>,
    pub contents: Vec<UserContent>,
    pub cancel_flag: Arc<AtomicBool>,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    pub approval_rx: mpsc::UnboundedReceiver<ApprovalNotification>,
    pub resolution_rx: mpsc::UnboundedReceiver<ApprovalResolution>,
    pub max_agent_turns: usize,
    pub invoke_agent_progress_slot: InvokeAgentProgressSlot,
}

/// Maps [`StreamChunk`] and [`InvokeAgentProgress`] events to [`AppEvent`]s.
struct TuiStreamHandler {
    event_tx: mpsc::UnboundedSender<AppEvent>,
    task_controller: AgentTaskController,
    pending_tool_names: HashMap<String, String>,
    pending_follow_up: Option<String>,
    cancelled: bool,
}

impl chatty_core::services::StreamChunkHandler for TuiStreamHandler {
    fn on_stream_started(&mut self) {
        let _ = self.event_tx.send(AppEvent::StreamStarted);
    }

    fn on_chunk(&mut self, chunk: Result<StreamChunk>) -> Result<ChunkAction> {
        match chunk? {
            StreamChunk::Text(text) => {
                let _ = self.event_tx.send(AppEvent::TextChunk(text));
                Ok(ChunkAction::Continue)
            }
            StreamChunk::ToolCallStarted { id, name } => {
                self.pending_tool_names.insert(id.clone(), name.clone());
                let _ = self.event_tx.send(AppEvent::ToolCallStarted { id, name });
                Ok(ChunkAction::Continue)
            }
            StreamChunk::ToolCallInput { id, arguments } => {
                let _ = self
                    .event_tx
                    .send(AppEvent::ToolCallInput { id, arguments });
                Ok(ChunkAction::Continue)
            }
            StreamChunk::ToolCallResult { id, result } => {
                if let Some(name) = self.pending_tool_names.remove(&id)
                    && let Some(prompt) = self.task_controller.observe_tool_result(&name)
                {
                    self.pending_follow_up = Some(prompt);
                    return Ok(ChunkAction::Break);
                }
                let _ = self.event_tx.send(AppEvent::ToolCallResult { id, result });
                Ok(ChunkAction::Continue)
            }
            StreamChunk::ToolCallError { id, error } => {
                if let Some(name) = self.pending_tool_names.remove(&id)
                    && let Some(prompt) = self.task_controller.observe_tool_result(&name)
                {
                    self.pending_follow_up = Some(prompt);
                    return Ok(ChunkAction::Break);
                }
                let _ = self.event_tx.send(AppEvent::ToolCallError { id, error });
                Ok(ChunkAction::Continue)
            }
            StreamChunk::ApprovalRequested {
                id,
                command,
                is_sandboxed,
            } => {
                let _ = self.event_tx.send(AppEvent::ApprovalRequested {
                    id,
                    command,
                    is_sandboxed,
                });
                Ok(ChunkAction::Continue)
            }
            StreamChunk::ApprovalResolved { id, approved } => {
                let _ = self
                    .event_tx
                    .send(AppEvent::ApprovalResolved { id, approved });
                Ok(ChunkAction::Continue)
            }
            StreamChunk::TokenUsage {
                input_tokens,
                output_tokens,
            } => {
                let _ = self.event_tx.send(AppEvent::TokenUsage {
                    input_tokens,
                    output_tokens,
                });
                Ok(ChunkAction::Continue)
            }
            StreamChunk::Done => Ok(ChunkAction::Break),
            StreamChunk::Error(e) => {
                let _ = self.event_tx.send(AppEvent::StreamError(e));
                Ok(ChunkAction::Break)
            }
        }
    }

    fn on_progress(&mut self, progress: InvokeAgentProgress) {
        match progress {
            InvokeAgentProgress::Started {
                agent_name,
                prompt,
                source,
            } => {
                let mode = match source {
                    chatty_core::models::message_types::ToolSource::Local => "local",
                    _ => "remote",
                };
                let label = format!("[{mode} agent: {agent_name}] {prompt}");
                let _ = self.event_tx.send(AppEvent::SubAgentProgress(label));
            }
            InvokeAgentProgress::Text(text) => {
                let _ = self.event_tx.send(AppEvent::SubAgentProgress(text));
            }
            InvokeAgentProgress::Finished { success, result } => {
                let message = if success {
                    result.unwrap_or_else(|| "Agent completed.".to_string())
                } else {
                    result.unwrap_or_else(|| "Agent failed.".to_string())
                };
                let _ = self.event_tx.send(AppEvent::SubAgentFinished(message));
            }
        }
    }

    fn on_cancelled(&mut self) {
        self.cancelled = true;
        let _ = self.event_tx.send(AppEvent::StreamCancelled);
    }

    fn on_stream_ended(&mut self) {
        let _ = self.event_tx.send(AppEvent::StreamCompleted);
        let follow_up = self.pending_follow_up.take().or_else(|| {
            (!self.cancelled)
                .then(|| self.task_controller.stream_end_follow_up())
                .flatten()
        });
        if let Some(prompt) = follow_up {
            let _ = self.event_tx.send(AppEvent::AgentProtocolFollowUp(prompt));
        }
    }
}

pub(super) async fn run_stream(params: StreamParams) -> Result<()> {
    let StreamParams {
        agent,
        history,
        contents,
        cancel_flag,
        event_tx,
        approval_rx,
        resolution_rx,
        max_agent_turns,
        invoke_agent_progress_slot,
    } = params;
    let task_controller = agent.task_controller();
    let (mut stream, _user_message) = stream_prompt(
        &agent,
        &history,
        contents,
        Some(approval_rx),
        Some(resolution_rx),
        max_agent_turns,
    )
    .await
    .context("Failed to start stream")?;

    let mut progress_rx =
        chatty_core::services::install_progress_channel(&invoke_agent_progress_slot);
    let mut handler = TuiStreamHandler {
        event_tx,
        task_controller,
        pending_tool_names: HashMap::new(),
        pending_follow_up: None,
        cancelled: false,
    };

    chatty_core::services::run_stream_loop(
        &mut stream,
        &mut progress_rx,
        &cancel_flag,
        &mut handler,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::services::{AgentTaskController, AgentTodoStatus, StreamChunkHandler};

    fn handler() -> (
        TuiStreamHandler,
        mpsc::UnboundedReceiver<AppEvent>,
        AgentTaskController,
    ) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let task_controller = AgentTaskController::new();
        (
            TuiStreamHandler {
                event_tx,
                task_controller: task_controller.clone(),
                pending_tool_names: HashMap::new(),
                pending_follow_up: None,
                cancelled: false,
            },
            event_rx,
            task_controller,
        )
    }

    #[test]
    fn emits_follow_up_after_repeated_tools_without_todos() {
        let (mut handler, mut event_rx, _controller) = handler();

        handler
            .on_chunk(Ok(StreamChunk::ToolCallStarted {
                id: "a".into(),
                name: "read_file".into(),
            }))
            .unwrap();
        handler
            .on_chunk(Ok(StreamChunk::ToolCallResult {
                id: "a".into(),
                result: "ok".into(),
            }))
            .unwrap();
        handler
            .on_chunk(Ok(StreamChunk::ToolCallStarted {
                id: "b".into(),
                name: "search_code".into(),
            }))
            .unwrap();
        let action = handler
            .on_chunk(Ok(StreamChunk::ToolCallResult {
                id: "b".into(),
                result: "ok".into(),
            }))
            .unwrap();
        assert!(matches!(action, ChunkAction::Break));

        handler.on_stream_ended();
        let events = drain_events(&mut event_rx);

        assert!(
            events
                .iter()
                .any(|event| matches!(event, AppEvent::AgentProtocolFollowUp(prompt) if prompt.contains("write_todos")))
        );
    }

    #[test]
    fn emits_follow_up_when_stream_ends_before_verification() {
        let (mut handler, mut event_rx, controller) = handler();
        controller
            .write_todos(
                "Ship".into(),
                vec![("t1".into(), "Implement".into(), "Implement change".into())],
            )
            .unwrap();
        controller
            .update_todo("t1".into(), AgentTodoStatus::Done, None, None)
            .unwrap();

        handler.on_stream_ended();
        let events = drain_events(&mut event_rx);

        assert!(
            events
                .iter()
                .any(|event| matches!(event, AppEvent::AgentProtocolFollowUp(prompt) if prompt.contains("verify_completion")))
        );
    }

    fn drain_events(event_rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> Vec<AppEvent> {
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        events
    }
}
