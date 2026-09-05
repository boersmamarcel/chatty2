use anyhow::Result;
use futures::StreamExt;
use futures::stream::BoxStream;
use rig_agent::agent::{MultiTurnStreamItem, StreamingError};
use rig_agent::streaming::StreamingPrompt;
use rig_core::completion::Message;
use rig_core::message::UserContent;
use rig_core::streaming::{StreamedAssistantContent, StreamedUserContent};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::factories::AgentClient;
use crate::models::clarification_store::{ClarificationNotification, ClarifyingQuestion};
use crate::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};
use crate::models::token_usage::ApiCallUsage;

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
    /// Usage for one completed provider request within the turn. Emitted once
    /// per request, so a turn with two tool calls yields three of these.
    ApiCallUsage(ApiCallUsage),
    /// Usage aggregated over every request in the turn, from the provider's
    /// final response. Arrives after the per-call chunks. `turn` is always 0
    /// (see [`normalize_usage`]'s aggregate convention).
    TurnUsage(ApiCallUsage),
    Done,
    Error(String),
}

/// How a provider reports cached prompt tokens relative to its input count.
///
/// Anthropic's native API reports `input_tokens` without cache reads and
/// writes; OpenAI-compatible usage (OpenRouter, Azure, Ollama, …) reports
/// `cached_tokens` as a subset of `prompt_tokens`. rig forwards each
/// provider's convention unchanged, so chatty normalises once here to
/// "input = uncached". Every provider chatty talks to today speaks the
/// OpenAI-compatible shape; the other variant exists so a native Anthropic
/// route can't silently double-count when one is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSemantics {
    /// `input_tokens` already excludes cached tokens (Anthropic native).
    InputExcludesCache,
    /// `input_tokens` includes cached tokens (OpenAI-compatible).
    InputIncludesCache,
}

/// Convert rig's per-provider usage into chatty's normalised per-call record.
///
/// `turn` is one-based; the aggregate built from a final response passes 0.
pub fn normalize_usage(
    semantics: UsageSemantics,
    turn: u32,
    usage: &rig_core::completion::Usage,
) -> ApiCallUsage {
    let cache_read = usage.cached_input_tokens;
    let cache_write = usage.cache_creation_input_tokens;
    let input = match semantics {
        UsageSemantics::InputExcludesCache => usage.input_tokens,
        UsageSemantics::InputIncludesCache => usage
            .input_tokens
            .saturating_sub(cache_read)
            .saturating_sub(cache_write),
    };
    let clamp = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    ApiCallUsage {
        turn,
        input_tokens: clamp(input),
        cache_read_tokens: clamp(cache_read),
        cache_write_tokens: clamp(cache_write),
        output_tokens: clamp(usage.output_tokens),
    }
}

/// Log one completed provider request's usage. This is the diagnostic for
/// prompt caching: a hit rate that stays low on the second request onwards
/// means the prompt prefix is changing between requests.
fn log_call_usage(call: &ApiCallUsage) {
    tracing::info!(
        turn = call.turn,
        input = call.input_tokens,
        cache_read = call.cache_read_tokens,
        cache_write = call.cache_write_tokens,
        output = call.output_tokens,
        hit_rate = call.cache_hit_rate().map(|r| (r * 100.0).round() as u32),
        "LLM completion call usage"
    );
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

/// Resolve a unique tool call id: the provider's `call_id` when it issued a
/// non-empty one, rig's internal correlation id otherwise. Shared by the
/// `ToolCall` and `ToolResult` arms of [`map_item`], which resolve the id the
/// same way so a call and its result carry the same chunk id.
fn resolve_call_id(
    provider: Option<&rig_core::completion::message::ProviderCallId>,
    internal_call_id: String,
) -> String {
    provider
        .map(|p| p.call_id.clone())
        .filter(|id| !id.is_empty())
        .unwrap_or(internal_call_id)
}

/// Map one item from the agent's multi-turn stream into the [`StreamChunk`]s
/// it produces.
///
/// A tool call yields two chunks (`ToolCallStarted` then `ToolCallInput`); a
/// tool result yields one (`ToolCallResult` or `ToolCallError`, depending on
/// [`tool_result_looks_like_error`]); a `CompletionCall` yields one
/// (`ApiCallUsage`); a `FinalResponse` yields one (`TokenUsage`). An item this
/// stream does not render (e.g. `ToolExecutionCommitted`, `ModelTurnRetried`,
/// a streamed delta) yields none.
fn map_item(item: MultiTurnStreamItem, semantics: UsageSemantics) -> Vec<StreamChunk> {
    match item {
        MultiTurnStreamItem::StreamAssistantItem(content) => match content {
            StreamedAssistantContent::Text(text) => vec![StreamChunk::Text(text.text)],
            StreamedAssistantContent::ToolCall {
                tool_call,
                internal_call_id,
            } => {
                let tool_id = resolve_call_id(tool_call.provider.as_ref(), internal_call_id);
                info!(
                    tool_id = %tool_id,
                    tool_name = %tool_call.function.name,
                    "ToolCall detected in stream"
                );
                vec![
                    StreamChunk::ToolCallStarted {
                        id: tool_id.clone(),
                        name: tool_call.function.name.clone(),
                    },
                    StreamChunk::ToolCallInput {
                        id: tool_id,
                        arguments: serde_json::to_string(&tool_call.function.arguments)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                ]
            }
            _ => Vec::new(),
        },
        MultiTurnStreamItem::StreamUserItem(user_content) => {
            let StreamedUserContent::ToolResult {
                tool_result,
                internal_call_id,
            } = user_content;
            let content_text = streamed_tool_result_to_text(&tool_result);
            let call_id = resolve_call_id(tool_result.provider.as_ref(), internal_call_id);

            if tool_result_looks_like_error(&content_text) {
                warn!(
                    tool_id = %call_id,
                    error = %content_text,
                    "ToolResult: Error detected"
                );
                vec![StreamChunk::ToolCallError {
                    id: call_id,
                    error: content_text,
                }]
            } else {
                info!(
                    tool_id = %call_id,
                    result_length = content_text.len(),
                    "ToolResult: Success"
                );
                vec![StreamChunk::ToolCallResult {
                    id: call_id,
                    result: content_text,
                }]
            }
        }
        MultiTurnStreamItem::CompletionCall(call) => {
            let call_usage = normalize_usage(semantics, call.call_index as u32 + 1, &call.usage);
            log_call_usage(&call_usage);
            vec![StreamChunk::ApiCallUsage(call_usage)]
        }
        MultiTurnStreamItem::FinalResponse(final_response) => {
            let usage = normalize_usage(semantics, 0, &final_response.usage());
            vec![StreamChunk::TurnUsage(usage)]
        }
        _ => Vec::new(),
    }
}

/// Map one line of the agent's stream — an item, or the transport `Err` that
/// ends it — into the chunks it produces, and whether the caller should stop
/// reading further items.
fn map_stream_result(
    item: Result<MultiTurnStreamItem, StreamingError>,
    semantics: UsageSemantics,
) -> (Vec<StreamChunk>, bool) {
    match item {
        Ok(item) => (map_item(item, semantics), false),
        Err(e) => (vec![StreamChunk::Error(e.to_string())], true),
    }
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
    let semantics = agent.provider().usage_semantics();

    let history_snapshot = history.to_vec();

    let mut agent_stream = agent
        .agent
        .stream_prompt(user_message.clone())
        .history(history_snapshot)
        .max_turns(max_agent_turns)
        .await;

    // A caller that does not wire a channel is treated the same as one whose
    // sender already dropped: `recv()` resolves to `None` immediately, which
    // disables that `select!` arm for the rest of the stream.
    let mut approval_rx = approval_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);
    let mut resolution_rx = resolution_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);
    let mut clarification_rx = clarification_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);

    let stream: ResponseStream = Box::pin(async_stream::stream! {
        loop {
            tokio::select! {
                item = agent_stream.next() => {
                    match item {
                        Some(result) => {
                            let (chunks, stop) = map_stream_result(result, semantics);
                            for chunk in chunks {
                                yield Ok(chunk);
                            }
                            if stop {
                                return;
                            }
                        }
                        None => {
                            yield Ok(StreamChunk::Done);
                            return;
                        }
                    }
                }

                // Process approval notifications
                Some(approval) = approval_rx.recv() => {
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
    });

    Ok((stream, user_message))
}

#[cfg(test)]
mod tests {
    use rig_agent::agent::CompletionCall;
    use rig_core::completion::CompletionError;
    use rig_core::completion::message::{
        ProviderCallId, Text, ToolCall, ToolCallId, ToolFunction, ToolResult, ToolResultContent,
    };

    use super::{
        MultiTurnStreamItem, StreamChunk, StreamedAssistantContent, StreamedUserContent,
        StreamingError, UsageSemantics, map_item, map_stream_result, normalize_usage,
        streamed_tool_result_to_text, tool_result_looks_like_error,
    };

    /// Anthropic reports `input_tokens` without the cached share; OpenAI-style
    /// usage reports it inside `prompt_tokens`. The same activity must yield
    /// the same normalised record either way, or the hit rate would mean
    /// something different per provider.
    #[test]
    fn usage_normalisation_agrees_across_provider_conventions() {
        let mut anthropic = rig_core::completion::Usage::new();
        anthropic.input_tokens = 200;
        anthropic.cached_input_tokens = 9_000;
        anthropic.cache_creation_input_tokens = 800;
        anthropic.output_tokens = 30;

        let mut openai_style = rig_core::completion::Usage::new();
        openai_style.input_tokens = 200 + 9_000 + 800;
        openai_style.cached_input_tokens = 9_000;
        openai_style.cache_creation_input_tokens = 800;
        openai_style.output_tokens = 30;

        let a = normalize_usage(UsageSemantics::InputExcludesCache, 2, &anthropic);
        let o = normalize_usage(UsageSemantics::InputIncludesCache, 2, &openai_style);
        assert_eq!(a, o);
        assert_eq!(a.input_tokens, 200);
        assert_eq!(a.cache_read_tokens, 9_000);
        assert_eq!(a.cache_write_tokens, 800);
        assert_eq!(a.prompt_tokens(), 10_000);
        assert_eq!(a.cache_hit_rate(), Some(0.9));
        assert_eq!(a.turn, 2);
    }

    #[test]
    fn usage_normalisation_never_underflows_on_inconsistent_counts() {
        let mut odd = rig_core::completion::Usage::new();
        odd.input_tokens = 10;
        odd.cached_input_tokens = 50;
        let n = normalize_usage(UsageSemantics::InputIncludesCache, 1, &odd);
        assert_eq!(n.input_tokens, 0);
        assert_eq!(n.cache_read_tokens, 50);
    }

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

    // -----------------------------------------------------------------
    // map_item (AGE-210 / finding B6)
    // -----------------------------------------------------------------

    #[test]
    fn map_item_maps_text() {
        let item = MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(
            Text::new("hello"),
        ));
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], StreamChunk::Text(t) if t == "hello"));
    }

    #[test]
    fn map_item_tool_call_prefers_provider_call_id() {
        let tool_call = ToolCall::from_wire(
            "call-99",
            ToolFunction::new("read_file".into(), serde_json::json!({"path": "README.md"})),
        );
        let item = MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
            tool_call,
            internal_call_id: "internal-1".into(),
        });
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 2);
        assert!(matches!(
            &chunks[0],
            StreamChunk::ToolCallStarted { id, name }
                if id == "call-99" && name == "read_file"
        ));
        assert!(matches!(
            &chunks[1],
            StreamChunk::ToolCallInput { id, arguments }
                if id == "call-99" && arguments.contains("README.md")
        ));
    }

    #[test]
    fn map_item_tool_call_falls_back_to_internal_call_id_without_a_provider_id() {
        let tool_call = ToolCall::new(
            ToolCallId::mint(),
            ToolFunction::new("search_code".into(), serde_json::json!({})),
        );
        let item = MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
            tool_call,
            internal_call_id: "internal-42".into(),
        });
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert!(matches!(
            &chunks[0],
            StreamChunk::ToolCallStarted { id, .. } if id == "internal-42"
        ));
    }

    #[test]
    fn map_item_tool_result_text_is_success() {
        let tool_result = ToolResult {
            call: ToolCallId::new("call-1").unwrap(),
            name: "read_file".into(),
            content: vec![ToolResultContent::text("# Chatty")],
            provider: ProviderCallId::new("call-1"),
        };
        let item = MultiTurnStreamItem::StreamUserItem(StreamedUserContent::tool_result(
            tool_result,
            "internal-1".into(),
        ));
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(
            &chunks[0],
            StreamChunk::ToolCallResult { id, result }
                if id == "call-1" && result == "# Chatty"
        ));
    }

    #[test]
    fn map_item_tool_result_error_prefix_becomes_tool_call_error() {
        let tool_result = ToolResult {
            call: ToolCallId::new("call-1").unwrap(),
            name: "run_shell".into(),
            content: vec![ToolResultContent::text(
                "Error: run_shell: exited with status 1",
            )],
            provider: None,
        };
        let item = MultiTurnStreamItem::StreamUserItem(StreamedUserContent::tool_result(
            tool_result,
            "internal-1".into(),
        ));
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(
            &chunks[0],
            StreamChunk::ToolCallError { id, error }
                if id == "internal-1" && error.starts_with("Error:")
        ));
    }

    #[test]
    fn map_item_tool_result_json_is_serialised_to_text() {
        let tool_result = ToolResult {
            call: ToolCallId::new("call-1").unwrap(),
            name: "query_data".into(),
            content: vec![ToolResultContent::json(serde_json::json!({"rows": 1}))],
            provider: None,
        };
        let item = MultiTurnStreamItem::StreamUserItem(StreamedUserContent::tool_result(
            tool_result,
            "internal-1".into(),
        ));
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert!(matches!(
            &chunks[0],
            StreamChunk::ToolCallResult { result, .. } if result.contains("\"rows\"")
        ));
    }

    #[test]
    fn map_item_completion_call_turn_is_one_based() {
        let item = MultiTurnStreamItem::CompletionCall(CompletionCall::new(
            1,
            rig_core::completion::Usage::new(),
        ));
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(
            &chunks[0],
            StreamChunk::ApiCallUsage(usage) if usage.turn == 2
        ));
    }

    #[test]
    fn map_item_final_response_yields_turn_usage() {
        let mut usage = rig_core::completion::Usage::new();
        usage.input_tokens = 100;
        usage.output_tokens = 20;
        let item = MultiTurnStreamItem::final_response(Vec::new(), usage);
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(
            &chunks[0],
            StreamChunk::TurnUsage(usage) if usage.input_tokens == 100 && usage.output_tokens == 20
        ));
    }

    #[test]
    fn map_stream_result_err_yields_error_and_ends_the_stream() {
        let err = StreamingError::Completion(CompletionError::ResponseError("boom".into()));
        let (chunks, stop) = map_stream_result(Err(err), UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], StreamChunk::Error(e) if e.contains("boom")));
        assert!(stop, "a transport error must stop the stream");
    }
}
