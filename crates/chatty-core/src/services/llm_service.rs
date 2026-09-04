use anyhow::Result;
use futures::StreamExt;
use futures::stream::BoxStream;
use rig_agent::agent::MultiTurnStreamItem;
use rig_agent::streaming::StreamingPrompt;
use rig_core::completion::Message;
use rig_core::message::UserContent;
use tokio::sync::mpsc;

use crate::factories::AgentClient;
use crate::models::clarification_store::{ClarificationNotification, ClarifyingQuestion};
use crate::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};

/// Stream chunks emitted during responses
#[derive(Debug, Clone)]
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
    ClarificationRequested {
        id: String,
        questions: Vec<ClarifyingQuestion>,
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

/// Whether a streamed tool result is reporting a failure.
///
/// The stream carries no error flag — `rig_core`'s streamed `ToolResult` has
/// only the content, and the typed `is_error()` lives on `rig_agent`'s
/// `ToolExecutionResult`, which never reaches here — so the text is the signal.
/// [`crate::tools::map_tool_error`] writes the `Error:` prefix this reads.
pub(crate) fn tool_result_looks_like_error(content_text: &str) -> bool {
    let trimmed = content_text.trim_start();
    trimmed.starts_with("Error:")
        || trimmed.starts_with("ERROR:")
        || trimmed.starts_with("error:")
        // Rig redacts many typed tool errors to this generic feedback.
        || trimmed.eq_ignore_ascii_case("the tool failed")
        || trimmed.contains("malformed JSON")
}

fn tool_result_content_to_text(
    content: &rig_core::completion::message::ToolResultContent,
) -> Option<String> {
    use rig_core::completion::message::ToolResultContent;

    match content {
        ToolResultContent::Text(text) => Some(text.text.clone()),
        ToolResultContent::Image(_) => Some("[Image result]".to_string()),
        ToolResultContent::Json { value } => serde_json::to_string(value).ok(),
    }
}

fn streamed_tool_result_to_text(tool_result: &rig_core::completion::message::ToolResult) -> String {
    tool_result
        .content
        .iter()
        .filter_map(tool_result_content_to_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Helper macro to process agent streams
macro_rules! process_agent_stream {
    ($stream:expr) => {
        Box::pin(async_stream::stream! {
            while let Some(item) = $stream.next().await {
                match item {
                    Ok(MultiTurnStreamItem::StreamAssistantItem(content)) => {
                        match content {
                            rig_core::streaming::StreamedAssistantContent::Text(text) => {
                                yield Ok(StreamChunk::Text(text.text));
                            }
                            rig_core::streaming::StreamedAssistantContent::ToolCall { tool_call, internal_call_id } => {
                                use tracing::info;
                                // Resolve a unique tool call ID.
                                // Priority: provider's call_id > rig's internal_call_id
                                let tool_id = tool_call
                                    .provider
                                    .as_ref()
                                    .map(|p| p.call_id.clone())
                                    .filter(|id| !id.is_empty())
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
                    Ok(MultiTurnStreamItem::StreamUserItem(user_content)) => {
                        use rig_core::streaming::StreamedUserContent;

                        let StreamedUserContent::ToolResult { tool_result, internal_call_id } = user_content;
                        let content_text = streamed_tool_result_to_text(&tool_result);

                        // Resolve a unique tool call ID (same logic as ToolCall arm above).
                        let call_id = tool_result
                            .provider
                            .as_ref()
                            .map(|p| p.call_id.clone())
                            .filter(|id| !id.is_empty())
                            .unwrap_or(internal_call_id);

                        let is_error = tool_result_looks_like_error(&content_text);

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
                    Ok(MultiTurnStreamItem::FinalResponse(final_response)) => {
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
    ($stream:expr, $approval_rx:expr, $resolution_rx:expr, $clarification_rx:expr) => {
        Box::pin(async_stream::stream! {
            let mut agent_stream = $stream;
            let mut approval_rx = $approval_rx;
            let mut resolution_rx = $resolution_rx;
            let mut clarification_rx = $clarification_rx;

            loop {
                tokio::select! {
                    // Process agent stream items
                    item = agent_stream.next() => {
                        match item {
                            Some(Ok(MultiTurnStreamItem::StreamAssistantItem(content))) => {
                                match content {
                                    rig_core::streaming::StreamedAssistantContent::Text(text) => {
                                        yield Ok(StreamChunk::Text(text.text));
                                    }
                                    rig_core::streaming::StreamedAssistantContent::ToolCall { tool_call, internal_call_id } => {
                                        use tracing::info;
                                        // Resolve a unique tool call ID.
                                        // Priority: provider's call_id > rig's internal_call_id
                                        let tool_id = tool_call
                                            .provider
                                            .as_ref()
                                            .map(|p| p.call_id.clone())
                                            .filter(|id| !id.is_empty())
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
                            Some(Ok(MultiTurnStreamItem::StreamUserItem(user_content))) => {
                                use rig_core::streaming::StreamedUserContent;

                                let StreamedUserContent::ToolResult { tool_result, internal_call_id } = user_content;
                                let content_text = streamed_tool_result_to_text(&tool_result);

                                // Resolve a unique tool call ID (same logic as ToolCall arm above).
                                let call_id = tool_result
                                    .provider
                                    .as_ref()
                                    .map(|p| p.call_id.clone())
                                    .filter(|id| !id.is_empty())
                                    .unwrap_or(internal_call_id);

                                let is_error = tool_result_looks_like_error(&content_text);

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
                            Some(Ok(MultiTurnStreamItem::FinalResponse(final_response))) => {
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

                    // Process clarifying-question notifications
                    Some(clarification) = clarification_rx.recv() => {
                        use tracing::debug;
                        debug!(
                            id = %clarification.id,
                            questions = clarification.questions.len(),
                            "Stream received clarification notification, emitting ClarificationRequested chunk"
                        );
                        yield Ok(StreamChunk::ClarificationRequested {
                            id: clarification.id,
                            questions: clarification.questions,
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
/// * `clarification_rx` - Optional receiver for clarifying-question notifications
///
/// # Returns
/// A tuple of (response_stream, user_message) where the stream contains the agent's response
pub async fn stream_prompt(
    agent: &AgentClient,
    history: &[Message],
    contents: Vec<UserContent>,
    approval_rx: Option<mpsc::UnboundedReceiver<ApprovalNotification>>,
    resolution_rx: Option<mpsc::UnboundedReceiver<ApprovalResolution>>,
    clarification_rx: Option<mpsc::UnboundedReceiver<ClarificationNotification>>,
    max_agent_turns: usize,
) -> Result<(ResponseStream, Message)> {
    let user_message = Message::User { content: contents };

    let history_snapshot = history.to_vec();

    let mut stream = agent
        .agent
        .stream_prompt(user_message.clone())
        .history(history_snapshot)
        .max_turns(max_agent_turns)
        .await;

    let stream: ResponseStream = if let (Some(approval_rx), Some(resolution_rx)) =
        (approval_rx, resolution_rx)
    {
        // A frontend that wires approvals wires clarifications too. If it
        // did not, the sender is dropped immediately and `recv()` yields
        // `None`, which disables that `select!` arm.
        let clarification_rx = clarification_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);
        process_agent_stream_with_approvals!(stream, approval_rx, resolution_rx, clarification_rx)
    } else {
        process_agent_stream!(stream)
    };

    Ok((stream, user_message))
}

#[cfg(test)]
mod tests {
    use rig_core::completion::message::{ToolCallId, ToolResult, ToolResultContent};

    use super::{streamed_tool_result_to_text, tool_result_looks_like_error};

    #[test]
    fn tool_result_looks_like_error_detects_rig_redacted_failures() {
        assert!(tool_result_looks_like_error("the tool failed"));
        assert!(tool_result_looks_like_error(
            "Error: Data array must not be empty"
        ));
        assert!(!tool_result_looks_like_error(
            r#"{"saved_path":"charts/sales.png"}"#
        ));
    }

    #[test]
    fn streamed_tool_result_serializes_json_payload_for_ui() {
        let tool_result = ToolResult {
            call: ToolCallId::new("call-1").unwrap(),
            name: "query_data".into(),
            content: vec![ToolResultContent::json(serde_json::json!({
                "markdown_table": "| a |\n| --- |\n| 1 |",
                "preview": {
                    "title": "query_data",
                    "columns": [{"name": "a", "data_type": "INTEGER"}],
                    "rows": [["1"]],
                    "row_count": 1,
                    "truncated": false,
                    "source": {"kind": "query", "sql": "SELECT 1"}
                }
            }))],
            provider: None,
        };

        let text = streamed_tool_result_to_text(&tool_result);
        assert!(text.contains("\"preview\""));
        assert!(text.contains("\"rows\""));
        assert!(!text.contains("[JSON result]"));
    }
}
