//! Stream processing: maps LLM stream chunks to TUI `AppEvent`s.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result};
use chatty_core::factories::AgentClient;
use chatty_core::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};
use chatty_core::services::{ChunkAction, StreamChunk, stream_prompt};
use chatty_core::tools::invoke_agent_tool::{InvokeAgentProgress, InvokeAgentProgressSlot};
use rig::message::UserContent;
use tokio::sync::mpsc;

use crate::events::AppEvent;

pub(super) struct StreamParams {
    pub agent: AgentClient,
    pub history: Vec<rig::completion::Message>,
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
                let _ = self.event_tx.send(AppEvent::ToolCallResult { id, result });
                Ok(ChunkAction::Continue)
            }
            StreamChunk::ToolCallError { id, error } => {
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
            InvokeAgentProgress::Started { agent_name, prompt } => {
                let label = format!("[Agent: {agent_name}] {prompt}");
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
        let _ = self.event_tx.send(AppEvent::StreamCancelled);
    }

    fn on_stream_ended(&mut self) {
        let _ = self.event_tx.send(AppEvent::StreamCompleted);
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
    let mut handler = TuiStreamHandler { event_tx };

    chatty_core::services::run_stream_loop(
        &mut stream,
        &mut progress_rx,
        &cancel_flag,
        &mut handler,
    )
    .await
}
