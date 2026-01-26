use anyhow::{Context, Result};
use futures::StreamExt;
use futures::stream::BoxStream;
use rig::OneOrMany;
use rig::completion::Message;
use rig::message::UserContent;
use rig::streaming::StreamingPrompt;

use crate::chatty::factories::AgentClient;

/// Stream chunks emitted during responses
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Text(String),
    ToolCallStarted { id: String, name: String },
    ToolCallInput { id: String, arguments: String },
    ToolCallResult { id: String, result: String },
    ToolCallError { id: String, error: String },
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
                            rig::streaming::StreamedAssistantContent::ToolCall(tool_call) => {
                                let tool_id = tool_call.call_id.clone()
                                    .unwrap_or_else(|| tool_call.id.clone());
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

                        let StreamedUserContent::ToolResult(tool_result) = user_content;
                        let content_text = tool_result.content.iter()
                            .filter_map(|c| match c {
                                ToolResultContent::Text(text) => Some(text.text.clone()),
                                ToolResultContent::Image(_) => Some("[Image result]".to_string()),
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        let call_id = tool_result.call_id.clone()
                            .unwrap_or_else(|| tool_result.id.clone());

                        let is_error = content_text.trim_start().starts_with("Error:")
                            || content_text.trim_start().starts_with("ERROR:")
                            || content_text.trim_start().starts_with("error:");

                        if is_error {
                            yield Ok(StreamChunk::ToolCallError {
                                id: call_id,
                                error: content_text,
                            });
                        } else {
                            yield Ok(StreamChunk::ToolCallResult {
                                id: call_id,
                                result: content_text,
                            });
                        }
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

/// Stream a prompt with an agent
///
/// # Arguments
/// * `agent` - The agent client to use
/// * `history` - Previous conversation messages
/// * `contents` - The user content to send
///
/// # Returns
/// A tuple of (response_stream, user_message) where the stream contains the agent's response
pub async fn stream_prompt(
    agent: &AgentClient,
    history: &[Message],
    contents: Vec<UserContent>,
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
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        AgentClient::OpenAI(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        AgentClient::Gemini(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        AgentClient::Cohere(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
        AgentClient::Ollama(agent) => {
            let mut stream = agent
                .stream_prompt(user_message.clone())
                .with_history(history_snapshot)
                .multi_turn(10)
                .await;
            process_agent_stream!(stream)
        }
    };

    Ok((stream, user_message))
}
