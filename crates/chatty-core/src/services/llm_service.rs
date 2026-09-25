use anyhow::Result;
use futures::StreamExt;
use futures::stream::BoxStream;
use rig_agent::agent::{MultiTurnStreamItem, StreamingError};
use rig_agent::completion::PromptError;
use rig_agent::streaming::StreamingPrompt;
use rig_core::completion::{CompletionError, Message};
use rig_core::message::UserContent;
use rig_core::streaming::{StreamedAssistantContent, StreamedUserContent};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::factories::AgentClient;
use crate::factories::agent_factory::RequestRecorder;
use crate::models::clarification_store::{ClarificationNotification, ClarifyingQuestion};
use crate::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};
use crate::models::token_usage::ApiCallUsage;
use crate::services::stream_processor::{StreamError, StreamErrorKind};
use crate::services::turn_budget::{TurnBudget, WRAP_UP_TOOL_CALL_STOP};

/// Stream chunks emitted during responses
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Text(String),
    /// A fragment of the model's reasoning, streamed ahead of its answer
    /// (`reasoning_content` on OpenAI-compatible wires, `<think>` blocks
    /// Ollama and vLLM split out). No frontend renders it today; it exists
    /// so the stall watchdog counts a thinking model as active — a model
    /// that reasons for longer than `STALL_TIMEOUT` used to be cut off as
    /// "stopped responding" while streaming the whole time (AGE-453).
    Reasoning(String),
    /// A fragment of a tool call's arguments still being streamed. The call
    /// arrives complete as `ToolCallStarted`/`ToolCallInput` once its
    /// arguments parse; this only marks the stream as live while a long
    /// argument — a whole file for `write_file`, say — is written (AGE-453).
    ToolCallDelta,
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
    /// rig's own record of the turn, in order: the prompt, each assistant
    /// tool-call message, each tool-result message, the final text. Arrives
    /// with the final response, before `Done`, so the frontends can persist
    /// the tool round-trips behind the final text (AGE-247). A run that
    /// fails with a provider error sends it too, just before its `Error`,
    /// holding what its last model call was sending (see
    /// `failed_run_messages`).
    TurnMessages(Vec<Message>),
    Done,
    Error(crate::services::stream_processor::StreamError),
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
/// (`ApiCallUsage`); a `FinalResponse` yields the turn's usage aggregate
/// (`TurnUsage`) and, when rig recorded the turn, its messages
/// (`TurnMessages`). A reasoning delta yields `Reasoning` and a tool-call
/// delta yields `ToolCallDelta`: neither is rendered, but each is one
/// `stream.next()` for the stall watchdog, which otherwise sees a model
/// thinking or writing a long tool argument as silence (AGE-453). An item
/// this stream does not render and that carries no liveness of its own
/// (e.g. `ToolExecutionCommitted`, `ModelTurnRetried`, the completed
/// `Reasoning` block that repeats its deltas) yields none.
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
            StreamedAssistantContent::ReasoningDelta { reasoning, .. } => {
                vec![StreamChunk::Reasoning(reasoning)]
            }
            StreamedAssistantContent::ToolCallDelta { .. } => vec![StreamChunk::ToolCallDelta],
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
            let mut chunks = vec![StreamChunk::TurnUsage(usage)];
            // rig's record of the turn, when the run supplied one: the prompt,
            // each assistant tool-call message, each tool-result message, the
            // final text. The frontends persist its tool round-trips behind the
            // final text entry (AGE-247). After the usage aggregate, before
            // `Done`. `map_item` is the single mapping path, so this one arm
            // replaces the two duplicated macro sites AGE-247 originally had.
            if let Some(messages) = final_response.messages {
                chunks.push(StreamChunk::TurnMessages(messages));
            }
            chunks
        }
        _ => Vec::new(),
    }
}

/// Classify one of rig's typed completion errors into a [`StreamErrorKind`]
/// (AGE-244 / D5), using the HTTP status the provider returned when there is
/// one.
fn classify_completion_error(err: &CompletionError) -> StreamErrorKind {
    if let Some(status) = err.provider_response_status() {
        return match status.as_u16() {
            401 | 403 => StreamErrorKind::Auth,
            429 => StreamErrorKind::RateLimited,
            code => StreamErrorKind::ProviderStatus(code),
        };
    }
    match err {
        // A malformed tool call surfaces from rig as a JSON parse failure on
        // the provider's response, not an HTTP status.
        CompletionError::JsonError(_) => StreamErrorKind::MalformedToolCall,
        _ => StreamErrorKind::Transport,
    }
}

/// Classify rig's `StreamingError` (the only typed error `stream_prompt`
/// ever sees) into a [`StreamErrorKind`] (AGE-244 / D5).
fn classify_streaming_error(err: &StreamingError) -> StreamErrorKind {
    match err {
        StreamingError::Completion(e) => classify_completion_error(e),
        StreamingError::Prompt(e) => match e.as_ref() {
            PromptError::CompletionError(e) => classify_completion_error(e),
            // A one-token tool-name mistake (AGE-497): worth a nudge, not an
            // outright stop, since rig's own error text already lists the
            // available/allowed tools the model can retry with.
            PromptError::UnknownToolCall { .. } => StreamErrorKind::UnknownToolCall,
            // Exhausted its turn budget, or was cancelled: neither is a
            // provider transport failure, and neither is worth a nudge.
            PromptError::MaxTurnsError { .. }
            | PromptError::PromptCancelled { .. }
            | PromptError::MemoryError(_) => StreamErrorKind::Other,
        },
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
        Err(e) => {
            let kind = classify_streaming_error(&e);
            (
                vec![StreamChunk::Error(StreamError::new(kind, e.to_string()))],
                true,
            )
        }
    }
}

/// Shown when a run's tool-free last call asked for a tool instead of
/// answering and produced no text of its own.
pub const BUDGET_SPENT_NOTE: &str = "[Stopped: this response's turn or time budget is spent and \
     tools were disabled for a final answer, but the model asked for another tool call instead \
     of answering.]";

/// The normal end of a run whose tool-free wrap-up call asked for a tool
/// anyway (see [`crate::services::turn_budget`]): rig reports it as
/// `MaxTurnsError` (a tool call left in the reasoning channel trips
/// `EmptyTurnRetry` into one call past the budget; "reached max turns
/// limit: 51" under `--max-agent-turns 50`) or as the budget hook's own
/// stop. Either way the model had its last word, so the run ends with it:
/// rig's record of this run's messages (past the `history_len` it started
/// with), a note when the last call gave no text, then `Done`. `None` for
/// every other error.
fn budget_spent_end(
    err: &StreamingError,
    history_len: usize,
    text_in_last_call: bool,
) -> Option<Vec<StreamChunk>> {
    let StreamingError::Prompt(e) = err else {
        return None;
    };
    let history: &[Message] = match e.as_ref() {
        PromptError::MaxTurnsError { chat_history, .. } => chat_history,
        PromptError::PromptCancelled {
            chat_history,
            reason,
        } if reason == WRAP_UP_TOOL_CALL_STOP => chat_history,
        _ => return None,
    };
    warn!(error = %e, "Budget spent on a call that asked for a tool; ending the run normally");
    let mut chunks = Vec::new();
    if let Some(messages) = history.get(history_len..).filter(|m| !m.is_empty()) {
        chunks.push(StreamChunk::TurnMessages(messages.to_vec()));
    }
    if !text_in_last_call {
        chunks.push(StreamChunk::Text(BUDGET_SPENT_NOTE.to_string()));
    }
    chunks.push(StreamChunk::Done);
    Some(chunks)
}

/// The messages of a run that failed with a provider error (transport, HTTP
/// status, malformed or unknown tool call): what its last model call was
/// sending, past the `history_len` the run started with, as the turn's
/// `TurnMessages`. rig hands a failed run's messages back to nobody, so
/// without this every tool round-trip of the run is lost and a retry
/// continues from a history that no longer shows the work (the offline
/// benchmark's "retry lost the task"). `None` for the errors that carry
/// their own record or end the run on purpose (`Other`: max turns,
/// cancellation), and when no call went out.
fn failed_run_messages(
    err: &StreamingError,
    recorder: &RequestRecorder,
    history_len: usize,
) -> Option<StreamChunk> {
    if classify_streaming_error(err) == StreamErrorKind::Other {
        return None;
    }
    let recorded = recorder.take()?;
    let messages = recorded.get(history_len..).filter(|m| !m.is_empty())?;
    Some(StreamChunk::TurnMessages(messages.to_vec()))
}

/// Stream a prompt with an agent
///
/// # Arguments
/// * `agent` - The agent client to use
/// * `history` - Previous conversation messages, moved into rig's `.history(..)`
/// * `contents` - The user content to send
/// * `approval_rx` - Optional receiver for approval notifications
/// * `resolution_rx` - Optional receiver for approval resolution notifications
/// * `clarification_rx` - Optional receiver for clarifying-question notifications
/// * `turn_budget` - The tool-turn budget of this call (`TurnBudget::new(max_agent_turns)`
///   unless the caller runs it as one pass of a longer run)
///
/// # Returns
/// The response stream. The caller already owns the `Vec<Message>` it built
/// for `history` and the `UserContent` it built for `contents`, so this no
/// longer hands back the `Message` it assembled from the latter.
pub async fn stream_prompt(
    agent: &AgentClient,
    history: Vec<Message>,
    contents: Vec<UserContent>,
    approval_rx: Option<mpsc::UnboundedReceiver<ApprovalNotification>>,
    resolution_rx: Option<mpsc::UnboundedReceiver<ApprovalResolution>>,
    clarification_rx: Option<mpsc::UnboundedReceiver<ClarificationNotification>>,
    turn_budget: TurnBudget,
) -> Result<ResponseStream> {
    let user_message = Message::User { content: contents };
    let semantics = agent.provider().usage_semantics();
    let history_len = history.len();
    let request_recorder = agent.request_recorder().clone();
    request_recorder.clear();

    // The turn budget sets rig's call cap (the tool turns plus one tool-free
    // wrap-up call) and tells the model how many tool turns it has left.
    let mut agent_stream = turn_budget
        .apply(agent.agent.stream_prompt(user_message).history(history))
        .await;

    // A caller that does not wire a channel is treated the same as one whose
    // sender already dropped: `recv()` resolves to `None` immediately, which
    // disables that `select!` arm for the rest of the stream.
    let mut approval_rx = approval_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);
    let mut resolution_rx = resolution_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);
    let mut clarification_rx = clarification_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);

    let stream: ResponseStream = Box::pin(async_stream::stream! {
        // Whether the model call in flight has said anything yet.
        let mut text_in_last_call = false;
        loop {
            tokio::select! {
                item = agent_stream.next() => {
                    match item {
                        Some(result) => {
                            if let Err(e) = &result
                                && let Some(chunks) = budget_spent_end(e, history_len, text_in_last_call)
                            {
                                for chunk in chunks {
                                    yield Ok(chunk);
                                }
                                return;
                            }
                            if let Err(e) = &result
                                && let Some(chunk) = failed_run_messages(e, &request_recorder, history_len)
                            {
                                yield Ok(chunk);
                            }
                            let (chunks, stop) = map_stream_result(result, semantics);
                            for chunk in chunks {
                                match &chunk {
                                    StreamChunk::Text(text) if !text.trim().is_empty() => {
                                        text_in_last_call = true;
                                    }
                                    // The next call starts after the results.
                                    StreamChunk::ToolCallResult { .. }
                                    | StreamChunk::ToolCallError { .. } => {
                                        text_in_last_call = false;
                                    }
                                    _ => {}
                                }
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

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use rig_agent::agent::CompletionCall;
    use rig_core::completion::CompletionError;
    use rig_core::completion::message::{
        ProviderCallId, Text, ToolCall, ToolCallId, ToolFunction, ToolResult, ToolResultContent,
    };
    use rig_core::streaming::ToolCallDeltaContent;

    use super::{
        BUDGET_SPENT_NOTE, Message, MultiTurnStreamItem, PromptError, RequestRecorder, StreamChunk,
        StreamErrorKind, StreamedAssistantContent, StreamedUserContent, StreamingError,
        UsageSemantics, WRAP_UP_TOOL_CALL_STOP, budget_spent_end, classify_completion_error,
        classify_streaming_error, failed_run_messages, map_item, map_stream_result,
        normalize_usage, streamed_tool_result_to_text, tool_result_looks_like_error,
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

    /// The wire field the hit rate actually comes from. The two tests above
    /// start from an already-parsed `Usage`, so neither would notice
    /// OpenRouter (or rig's mapping of it) moving `cached_tokens` — and a
    /// silently-zero cache read is indistinguishable from a cache that is not
    /// working, which is the thing ADR-0010's threshold is measured against
    /// (AGE-278).
    #[test]
    fn openrouter_cached_tokens_reach_the_normalised_record() {
        let reported: rig_core::providers::openrouter::Usage = serde_json::from_str(
            r#"{"prompt_tokens":5000,"completion_tokens":120,"total_tokens":5120,
                "prompt_tokens_details":{"cached_tokens":1200,"cache_write_tokens":300}}"#,
        )
        .expect("an OpenRouter usage block deserializes");
        let call = normalize_usage(
            UsageSemantics::InputIncludesCache,
            1,
            &rig_core::completion::Usage::from(&reported),
        );
        assert_eq!(call.cache_read_tokens, 1_200);
        assert_eq!(call.cache_write_tokens, 300);
        // OpenRouter counts both inside `prompt_tokens`; chatty stores the
        // uncached share, so the three still add back to the whole prompt.
        assert_eq!(call.input_tokens, 3_500);
        assert_eq!(call.prompt_tokens(), 5_000);

        let uncached: rig_core::providers::openrouter::Usage = serde_json::from_str(
            r#"{"prompt_tokens":5000,"completion_tokens":120,"total_tokens":5120}"#,
        )
        .expect("usage without a details block deserializes");
        let call = normalize_usage(
            UsageSemantics::InputIncludesCache,
            1,
            &rig_core::completion::Usage::from(&uncached),
        );
        assert_eq!(call.cache_read_tokens, 0);
        assert_eq!(call.input_tokens, 5_000);
        assert_eq!(call.cache_hit_rate(), Some(0.0));
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

    /// AGE-453: a thinking model streams `reasoning_content` deltas for
    /// minutes before its first answer token. Each must reach the stream
    /// loop as a chunk, or the stall watchdog ends the turn as "stopped
    /// responding" while the provider is busy the whole time.
    #[test]
    fn map_item_maps_reasoning_delta_to_a_chunk() {
        let item =
            MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ReasoningDelta {
                id: "r-1".into(),
                provider_id: None,
                reasoning: "Let me think".into(),
            });
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(&chunks[0], StreamChunk::Reasoning(t) if t == "Let me think"));
    }

    /// AGE-453: the arguments of a long tool call (a whole file for
    /// `write_file`) stream for as long as the model takes to write them;
    /// the completed `ToolCall` only arrives at the end. The deltas must
    /// count as activity in the meantime.
    #[test]
    fn map_item_maps_tool_call_delta_to_a_chunk() {
        for content in [
            ToolCallDeltaContent::Name("write_file".into()),
            ToolCallDeltaContent::Delta("{\"path\": \"src/".into()),
        ] {
            let item =
                MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCallDelta {
                    internal_call_id: "internal-1".into(),
                    content,
                });
            let chunks = map_item(item, UsageSemantics::InputIncludesCache);
            assert_eq!(chunks.len(), 1);
            assert!(matches!(&chunks[0], StreamChunk::ToolCallDelta));
        }
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
        assert!(matches!(
            &chunks[0],
            StreamChunk::Error(e) if e.message.contains("boom") && e.kind == StreamErrorKind::Transport
        ));
        assert!(stop, "a transport error must stop the stream");
    }

    // -------------------------------------------------------------------
    // Error classification (AGE-244 / D5): one rig error per kind maps
    // correctly to a StreamErrorKind.
    // -------------------------------------------------------------------

    fn status_error(code: u16) -> CompletionError {
        use rig_core::http_client;
        CompletionError::HttpError(http_client::Error::InvalidStatusCode(
            reqwest::StatusCode::from_u16(code).unwrap(),
        ))
    }

    #[test]
    fn classifies_401_as_auth() {
        assert_eq!(
            classify_completion_error(&status_error(401)),
            StreamErrorKind::Auth
        );
    }

    #[test]
    fn classifies_403_as_auth() {
        assert_eq!(
            classify_completion_error(&status_error(403)),
            StreamErrorKind::Auth
        );
    }

    #[test]
    fn classifies_429_as_rate_limited() {
        assert_eq!(
            classify_completion_error(&status_error(429)),
            StreamErrorKind::RateLimited
        );
    }

    #[test]
    fn classifies_5xx_as_provider_status() {
        assert_eq!(
            classify_completion_error(&status_error(503)),
            StreamErrorKind::ProviderStatus(503)
        );
    }

    #[test]
    fn classifies_json_error_as_malformed_tool_call() {
        let json_err = serde_json::from_str::<serde_json::Value>("{not json").unwrap_err();
        assert_eq!(
            classify_completion_error(&CompletionError::JsonError(json_err)),
            StreamErrorKind::MalformedToolCall
        );
    }

    #[test]
    fn classifies_response_error_with_no_status_as_transport() {
        assert_eq!(
            classify_completion_error(&CompletionError::ResponseError("boom".into())),
            StreamErrorKind::Transport
        );
    }

    #[test]
    fn classifies_max_turns_as_other() {
        let err = StreamingError::Prompt(Box::new(PromptError::MaxTurnsError {
            max_turns: 10,
            chat_history: Box::new(Vec::new()),
            prompt: Box::new(Message::user("hi")),
        }));
        assert_eq!(classify_streaming_error(&err), StreamErrorKind::Other);
    }

    /// AGE-497: a hallucinated tool name is worth a nudge, not the same
    /// outright `Stop` as max-turns/cancellation/memory errors.
    #[test]
    fn classifies_unknown_tool_call_distinctly_from_other() {
        let err = StreamingError::Prompt(Box::new(PromptError::UnknownToolCall {
            tool_name: "web_search".to_string(),
            available_tools: vec!["search_web".to_string()],
            allowed_tools: vec!["search_web".to_string()],
            chat_history: Box::new(Vec::new()),
        }));
        assert_eq!(
            classify_streaming_error(&err),
            StreamErrorKind::UnknownToolCall
        );
    }

    #[test]
    fn classifies_prompt_completion_error_by_delegating() {
        let err = StreamingError::Prompt(Box::new(PromptError::CompletionError(status_error(401))));
        assert_eq!(classify_streaming_error(&err), StreamErrorKind::Auth);
    }

    fn chunk_names(chunks: &[StreamChunk]) -> Vec<String> {
        chunks
            .iter()
            .map(|c| match c {
                StreamChunk::TurnMessages(m) => format!("TurnMessages({})", m.len()),
                StreamChunk::Text(t) => format!("Text({t})"),
                StreamChunk::Done => "Done".to_string(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// "MaxTurnsError: reached max turns limit: 51" ended a
    /// `--max-agent-turns 50` run as a failure when its tool-free last call
    /// asked for a tool anyway. It now ends the run normally, with the
    /// run's own messages (not the history it started with) and the
    /// model's text, or a note when there was none.
    #[test]
    fn a_budget_spent_on_a_tool_call_ends_the_run_normally() {
        let history = vec![
            Message::user("earlier question"),
            Message::assistant("earlier answer"),
            Message::user("the task"),
            Message::assistant("partial work"),
        ];
        let max_turns = StreamingError::Prompt(Box::new(PromptError::MaxTurnsError {
            max_turns: 51,
            chat_history: Box::new(history.clone()),
            prompt: Box::new(Message::user("tool results")),
        }));
        assert_eq!(
            chunk_names(&budget_spent_end(&max_turns, 2, true).unwrap()),
            vec!["TurnMessages(2)", "Done"]
        );
        assert_eq!(
            chunk_names(&budget_spent_end(&max_turns, 2, false).unwrap()),
            vec![
                "TurnMessages(2)".to_string(),
                format!("Text({BUDGET_SPENT_NOTE})"),
                "Done".to_string()
            ]
        );

        let stopped = StreamingError::Prompt(Box::new(PromptError::PromptCancelled {
            chat_history: history,
            reason: WRAP_UP_TOOL_CALL_STOP.to_string(),
        }));
        assert_eq!(
            chunk_names(&budget_spent_end(&stopped, 4, true).unwrap()),
            vec!["Done"]
        );
    }

    /// Any other error, a user's cancel included, is still an error.
    #[test]
    fn other_errors_are_not_budget_ends() {
        let cancelled = StreamingError::Prompt(Box::new(PromptError::PromptCancelled {
            chat_history: Vec::new(),
            reason: "user stop".to_string(),
        }));
        assert!(budget_spent_end(&cancelled, 0, true).is_none());
        let transport = StreamingError::Completion(CompletionError::ProviderError("boom".into()));
        assert!(budget_spent_end(&transport, 0, true).is_none());
    }

    /// A run that failed on the wire hands on the round-trips its last
    /// request carried, past the history it started with; an error that
    /// ends a run on purpose does not.
    #[test]
    fn a_failed_run_hands_on_what_its_last_call_was_sending() {
        let earlier = Message::user("an earlier turn");
        let prompt = Message::user("the task");
        let call = Message::Assistant {
            id: None,
            content: vec![rig_core::message::AssistantContent::ToolCall(
                ToolCall::new(
                    ToolCallId::new("call_1").unwrap(),
                    ToolFunction::new("read_file".into(), serde_json::json!({"path": "a"})),
                ),
            )],
        };
        let result = Message::User {
            content: vec![rig_core::message::UserContent::ToolResult(ToolResult {
                call: ToolCallId::new("call_1").unwrap(),
                name: "read_file".into(),
                content: vec![ToolResultContent::text("contents")],
                provider: None,
            })],
        };
        let recorder = RequestRecorder::default();
        let transport = StreamingError::Completion(CompletionError::ProviderError(
            "Http client error: error sending request for url".into(),
        ));

        recorder.record(vec![
            earlier.clone(),
            prompt.clone(),
            call.clone(),
            result.clone(),
        ]);
        match failed_run_messages(&transport, &recorder, 1) {
            Some(StreamChunk::TurnMessages(messages)) => {
                assert_eq!(messages, vec![prompt.clone(), call.clone(), result.clone()]);
            }
            other => panic!("expected the run's messages, got {other:?}"),
        }
        assert!(
            failed_run_messages(&transport, &recorder, 1).is_none(),
            "the record is handed on once"
        );

        recorder.record(vec![earlier, prompt, call, result]);
        let cancelled = StreamingError::Prompt(Box::new(PromptError::PromptCancelled {
            chat_history: Vec::new(),
            reason: "stop".into(),
        }));
        assert!(failed_run_messages(&cancelled, &recorder, 1).is_none());
    }
}
