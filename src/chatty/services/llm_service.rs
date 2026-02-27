use anyhow::{Context, Result};
use futures::StreamExt;
use futures::stream::BoxStream;
use rig::OneOrMany;
use rig::completion::Message;
use rig::message::UserContent;
use rig::streaming::StreamingPrompt;
use tokio::sync::mpsc;

use crate::chatty::factories::AgentClient;
use crate::chatty::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};

/// Stream chunks emitted during responses
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum StreamChunk {
    Text(String),
    ToolCallStarted {
        id: String,
        name: String,
    },
    ToolCallInput {
        id: String,
        arguments: String,
    },
    ToolCallResult {
        id: String,
        result: String,
    },
    ToolCallError {
        id: String,
        error: String,
    },
    ApprovalRequested {
        id: String,
        command: String,
        is_sandboxed: bool,
    },
    ApprovalResolved {
        id: String,
        approved: bool,
    },
    TokenUsage {
        input_tokens: u32,
        output_tokens: u32,
    },
    Done,
    Error(String),
}

/// Type alias for response streams
pub type ResponseStream = BoxStream<'static, Result<StreamChunk>>;

/// Helper macro to process agent streams
macro_rules! process_agent_stream {
    ($stream:expr) => {
        Box::pin(async_stream::stream! {
            while let Some(item) = $stream.next().await {
                match item {
                    Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(content)) => {
                        match content {
                            rig::streaming::StreamedAssistantContent::Text(text) => {
                                yield Ok(StreamChunk::Text(text.text));
                            }
                            rig::streaming::StreamedAssistantContent::ToolCall { tool_call, internal_call_id } => {
                                use tracing::info;
                                // Use call_id > non-empty id > internal_call_id as fallback.
                                // Ollama returns empty id and no call_id, so internal_call_id
                                // (a unique nanoid from rig-core) is needed to correctly
                                // correlate ToolCallStarted/Input/Result events in the UI.
                                let tool_id = tool_call.call_id.clone()
                                    .or_else(|| if tool_call.id.is_empty() { None } else { Some(tool_call.id.clone()) })
                                    .unwrap_or_else(|| internal_call_id.clone());
                                info!(
                                    tool_id = %tool_id,
                                    tool_name = %tool_call.function.name,
                                    "ToolCall detected in stream"
                                );
                                yield Ok(StreamChunk::ToolCallStarted {
                                    id: tool_id.clone(),
                                    name: tool_call.function.name.clone(),
                                });
                                yield Ok(StreamChunk::ToolCallInput {
                                    id: tool_id,
                                    arguments: serde_json::to_string(&tool_call.function.arguments)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                });
                            }
                            _ => {}
                        }
                    }
                    Ok(rig::agent::MultiTurnStreamItem::StreamUserItem(user_content)) => {
                        use rig::streaming::StreamedUserContent;
                        use rig::completion::message::ToolResultContent;

                        let StreamedUserContent::ToolResult { tool_result, internal_call_id } = user_content;
                        let content_text = tool_result.content.iter()
                            .filter_map(|c| match c {
                                ToolResultContent::Text(text) => Some(text.text.clone()),
                                ToolResultContent::Image(_) => Some("[Image result]".to_string()),
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        // Same fallback chain as ToolCall: call_id > non-empty id > internal_call_id
                        let call_id = tool_result.call_id.clone()
                            .or_else(|| if tool_result.id.is_empty() { None } else { Some(tool_result.id.clone()) })
                            .unwrap_or_else(|| internal_call_id.clone());

                        let is_error = content_text.trim_start().starts_with("Error:")
                            || content_text.trim_start().starts_with("ERROR:")
                            || content_text.trim_start().starts_with("error:");

                        if is_error {
                            use tracing::warn;
                            warn!(
                                tool_id = %call_id,
                                error = %content_text,
                                "ToolResult: Error detected"
                            );
                            yield Ok(StreamChunk::ToolCallError {
                                id: call_id,
                                error: content_text,
                            });
                        } else {
                            use tracing::info;
                            info!(
                                tool_id = %call_id,
                                result_length = content_text.len(),
                                "ToolResult: Success"
                            );
                            yield Ok(StreamChunk::ToolCallResult {
                                id: call_id,
                                result: content_text,
                            });
                        }
                    }
                    Ok(rig::agent::MultiTurnStreamItem::FinalResponse(final_response)) => {
                        // Extract token usage from the final response
                        let usage = final_response.usage();
                        let input_tokens = usage.input_tokens as u32;
                        let output_tokens = usage.output_tokens as u32;
                        yield Ok(StreamChunk::TokenUsage {
                            input_tokens,
                            output_tokens,
                        });
                    }
                    Err(e) => {
                        yield Ok(StreamChunk::Error(e.to_string()));
                        return;
                    }
                    _ => {}
                }
            }
            yield Ok(StreamChunk::Done);
        })
    };
}

/// Helper macro to process agent streams with approval notifications
macro_rules! process_agent_stream_with_approvals {
    ($stream:expr, $approval_rx:expr, $resolution_rx:expr) => {
        Box::pin(async_stream::stream! {
            let mut agent_stream = $stream;
            let mut approval_rx = $approval_rx;
            let mut resolution_rx = $resolution_rx;

            loop {
                tokio::select! {
                    // Process agent stream items
                    item = agent_stream.next() => {
                        match item {
                            Some(Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(content))) => {
                                match content {
                                    rig::streaming::StreamedAssistantContent::Text(text) => {
                                        yield Ok(StreamChunk::Text(text.text));
                                    }
                                    rig::streaming::StreamedAssistantContent::ToolCall { tool_call, internal_call_id } => {
                                        use tracing::info;
                                        let tool_id = tool_call.call_id.clone()
                                            .or_else(|| if tool_call.id.is_empty() { None } else { Some(tool_call.id.clone()) })
                                            .unwrap_or_else(|| internal_call_id.clone());
                                        info!(
                                            tool_id = %tool_id,
                                            tool_name = %tool_call.function.name,
                                            "ToolCall detected in stream"
                                        );
                                        yield Ok(StreamChunk::ToolCallStarted {
                                            id: tool_id.clone(),
                                            name: tool_call.function.name.clone(),
                                        });
                                        yield Ok(StreamChunk::ToolCallInput {
                                            id: tool_id,
                                            arguments: serde_json::to_string(&tool_call.function.arguments)
                                                .unwrap_or_else(|_| "{}".to_string()),
                                        });
                                    }
                                    _ => {}
                                }
                            }
                            Some(Ok(rig::agent::MultiTurnStreamItem::StreamUserItem(user_content))) => {
                                use rig::streaming::StreamedUserContent;
                                use rig::completion::message::ToolResultContent;

                                let StreamedUserContent::ToolResult { tool_result, internal_call_id } = user_content;
                                let content_text = tool_result.content.iter()
                                    .filter_map(|c| match c {
                                        ToolResultContent::Text(text) => Some(text.text.clone()),
                                        ToolResultContent::Image(_) => Some("[Image result]".to_string()),
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");

                                let call_id = tool_result.call_id.clone()
                                    .or_else(|| if tool_result.id.is_empty() { None } else { Some(tool_result.id.clone()) })
                                    .unwrap_or_else(|| internal_call_id.clone());

                                let is_error = content_text.trim_start().starts_with("Error:")
                                    || content_text.trim_start().starts_with("ERROR:")
                                    || content_text.trim_start().starts_with("error:");

                                if is_error {
                                    use tracing::warn;
                                    warn!(
                                        tool_id = %call_id,
                                        error = %content_text,
                                        "ToolResult: Error detected"
                                    );
                                    yield Ok(StreamChunk::ToolCallError {
                                        id: call_id,
                                        error: content_text,
                                    });
                                } else {
                                    use tracing::info;
                                    info!(
                                        tool_id = %call_id,
                                        result_length = content_text.len(),
                                        "ToolResult: Success"
                                    );
                                    yield Ok(StreamChunk::ToolCallResult {
                                        id: call_id,
                                        result: content_text,
                                    });
                                }
                            }
                            Some(Ok(rig::agent::MultiTurnStreamItem::FinalResponse(final_response))) => {
                                let usage = final_response.usage();
                                yield Ok(StreamChunk::TokenUsage {
                                    input_tokens: usage.input_tokens as u32,
                                    output_tokens: usage.output_tokens as u32,
                                });
                            }
                            Some(Err(e)) => {
                                yield Ok(StreamChunk::Error(e.to_string()));
                                return;
                            }
                            None => {
                                yield Ok(StreamChunk::Done);
                                return;
                            }
                            _ => {}
                        }
                    }

                    // Process approval notifications
                    Some(approval) = approval_rx.recv() => {
                        use tracing::debug;
                        debug!(
                            id = %approval.id,
                            command = %approval.command,
                            sandboxed = approval.is_sandboxed,
                            "Stream received approval notification, emitting ApprovalRequested chunk"
                        );
                        yield Ok(StreamChunk::ApprovalRequested {
                            id: approval.id,
                            command: approval.command,
                            is_sandboxed: approval.is_sandboxed,
                        });
                    }

                    // Process resolution notifications
                    Some(resolution) = resolution_rx.recv() => {
                        use tracing::debug;
                        debug!(
                            id = %resolution.id,
                            approved = resolution.approved,
                            "Stream received resolution notification, emitting ApprovalResolved chunk"
                        );
                        yield Ok(StreamChunk::ApprovalResolved {
                            id: resolution.id,
                            approved: resolution.approved,
                        });
                    }
                }
            }
        })
    };
}

/// Stream a prompt with an agent
///
/// # Arguments
/// * `agent` - The agent client to use
/// * `history` - Previous conversation messages
/// * `contents` - The user content to send
/// * `approval_rx` - Optional receiver for approval notifications
/// * `resolution_rx` - Optional receiver for approval resolution notifications
///
/// # Returns
/// A tuple of (response_stream, user_message) where the stream contains the agent's response
pub async fn stream_prompt(
    agent: &AgentClient,
    history: &[Message],
    contents: Vec<UserContent>,
    approval_rx: Option<mpsc::UnboundedReceiver<ApprovalNotification>>,
    resolution_rx: Option<mpsc::UnboundedReceiver<ApprovalResolution>>,
    max_agent_turns: usize,
) -> Result<(ResponseStream, Message)> {
    let user_message = Message::User {
        content: OneOrMany::many(contents).context("Failed to create message from contents")?,
    };

    let history_snapshot = history.to_vec();

    let stream: ResponseStream = match agent {
        AgentClient::Anthropic(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(max_agent_turns)
                .await;

            if let (Some(approval_rx), Some(resolution_rx)) = (approval_rx, resolution_rx) {
                process_agent_stream_with_approvals!(stream, approval_rx, resolution_rx)
            } else {
                process_agent_stream!(stream)
            }
        }
        AgentClient::OpenAI(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(max_agent_turns)
                .await;

            if let (Some(approval_rx), Some(resolution_rx)) = (approval_rx, resolution_rx) {
                process_agent_stream_with_approvals!(stream, approval_rx, resolution_rx)
            } else {
                process_agent_stream!(stream)
            }
        }
        AgentClient::Gemini(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(max_agent_turns)
                .await;

            if let (Some(approval_rx), Some(resolution_rx)) = (approval_rx, resolution_rx) {
                process_agent_stream_with_approvals!(stream, approval_rx, resolution_rx)
            } else {
                process_agent_stream!(stream)
            }
        }
        AgentClient::Mistral(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(max_agent_turns)
                .await;

            if let (Some(approval_rx), Some(resolution_rx)) = (approval_rx, resolution_rx) {
                process_agent_stream_with_approvals!(stream, approval_rx, resolution_rx)
            } else {
                process_agent_stream!(stream)
            }
        }
        AgentClient::Ollama(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(max_agent_turns)
                .await;

            if let (Some(approval_rx), Some(resolution_rx)) = (approval_rx, resolution_rx) {
                process_agent_stream_with_approvals!(stream, approval_rx, resolution_rx)
            } else {
                process_agent_stream!(stream)
            }
        }
        AgentClient::AzureOpenAI(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(max_agent_turns)
                .await;

            if let (Some(approval_rx), Some(resolution_rx)) = (approval_rx, resolution_rx) {
                process_agent_stream_with_approvals!(stream, approval_rx, resolution_rx)
            } else {
                process_agent_stream!(stream)
            }
        }
    };

    Ok((stream, user_message))
}
