use anyhow::Result;
use futures::StreamExt;
use futures::stream::BoxStream;
use rig_agent::agent::MultiTurnStreamItem;
use rig_agent::completion::PromptError;
use rig_core::ErrorKind;
use rig_core::completion::Message;
use rig_core::error::ProviderError;
use rig_core::message::{Text, ToolResultContent, UserContent};
use rig_core::streaming::{Item, PartKind, StreamEvent};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::factories::AgentClient;
use crate::factories::agent_factory::RequestRecorder;
use crate::models::clarification_store::{ClarificationNotification, ClarifyingQuestion};
use crate::models::execution_approval_store::{ApprovalNotification, ApprovalResolution};
use crate::models::token_usage::{ApiCallUsage, ModelRef};
use crate::services::budget_stop::{BUDGET_STOP, StopOnBudgetExhausted, endpoint_refusal};
use crate::services::cut_off_tool_call::{ArgumentTail, CutOffCalls, MAX_CUT_OFF_RECOVERIES};
use crate::services::stream_processor::{StreamError, StreamErrorKind};
use crate::services::turn_budget::{TurnBudget, WRAP_UP_TOOL_CALL_STOP};
use crate::services::worker_start::{StopOnWorkerStartFailure, WORKER_START_STOP};

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
        /// What the approval is for, as it goes up a call chain (AGE-646).
        detail: crate::models::execution_approval_store::ApprovalDetail,
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
    /// Usage for a call the turn made outside the model loop: a context
    /// compaction's summary call (AGE-683), on the model that served it.
    /// Counted with the turn, but not one of the requests the provider's
    /// aggregate covers.
    CompactionUsage(ApiCallUsage),
    /// Usage for one `llm::complete` call a plugin tool made during the turn
    /// (PL-U2, AGE-616), on the model that served it. Not one of the
    /// requests the provider's aggregate covers.
    PluginUsage {
        plugin: String,
        call: ApiCallUsage,
    },
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
/// A counter the provider did not report (rig 0.46's `None`) counts as zero,
/// as rig 0.42's zero did.
pub fn normalize_usage(
    semantics: UsageSemantics,
    turn: u32,
    usage: &rig_core::completion::Usage,
) -> ApiCallUsage {
    let cache_read = usage.cached_input_tokens.unwrap_or(0);
    let cache_write = usage.cache_creation_input_tokens.unwrap_or(0);
    let reported_input = usage.input_tokens.unwrap_or(0);
    let input = match semantics {
        UsageSemantics::InputExcludesCache => reported_input,
        UsageSemantics::InputIncludesCache => reported_input
            .saturating_sub(cache_read)
            .saturating_sub(cache_write),
    };
    let clamp = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    ApiCallUsage {
        turn,
        input_tokens: clamp(input),
        cache_read_tokens: clamp(cache_read),
        cache_write_tokens: clamp(cache_write),
        output_tokens: clamp(usage.output_tokens.unwrap_or(0)),
        reasoning_tokens: clamp(usage.reasoning_tokens.unwrap_or(0)),
        ..Default::default()
    }
}

/// Stamps the stream's usage records with the facts only the stream knows
/// (AGE-682): the model that served them, when they finished and how long
/// they took.
struct UsageClock {
    model: ModelRef,
    stream_started: Instant,
    call_started: Instant,
}

impl UsageClock {
    fn new(model: ModelRef) -> Self {
        let now = Instant::now();
        Self {
            model,
            stream_started: now,
            call_started: now,
        }
    }

    fn stamp(&mut self, chunk: &mut StreamChunk) {
        let (usage, started) = match chunk {
            StreamChunk::ApiCallUsage(call) => (call, self.call_started),
            StreamChunk::TurnUsage(aggregate) => (aggregate, self.stream_started),
            // The next request goes out once the tool results are in.
            StreamChunk::ToolCallResult { .. } | StreamChunk::ToolCallError { .. } => {
                self.call_started = Instant::now();
                return;
            }
            _ => return,
        };
        usage.model = Some(self.model.clone());
        usage.at = Some(SystemTime::now());
        usage.duration_ms = started.elapsed().as_millis() as u64;
        if usage.turn > 0 {
            self.call_started = Instant::now();
        }
    }

    /// A compaction's summary call ran inside the next model call's window
    /// and is timed on its own record, so that window starts after it.
    fn exclude(&mut self, compaction: &ApiCallUsage) {
        let shifted = self.call_started + Duration::from_millis(compaction.duration_ms);
        self.call_started = shifted.min(Instant::now());
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

/// Whether a streamed tool result's text is reporting a failure.
///
/// rig 0.46's streamed `ToolResult` carries `is_error` for a tool that
/// failed, was refused or never ran; a tool that reports its own failure as
/// text does not set it, so the text stays a signal too.
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
        MultiTurnStreamItem::StreamAssistantItem(Item::Event(event)) => match event {
            StreamEvent::Text { text, .. } => vec![StreamChunk::Text(text)],
            StreamEvent::Reasoning { text, .. } => vec![StreamChunk::Reasoning(text)],
            StreamEvent::Start {
                kind: PartKind::ToolCall,
                ..
            }
            | StreamEvent::Arguments { .. } => vec![StreamChunk::ToolCallDelta],
            _ => Vec::new(),
        },
        MultiTurnStreamItem::ToolCall { tool_call } => {
            let tool_id = tool_call.id.to_string();
            info!(
                tool_id = %tool_id,
                tool_name = %tool_call.function.name,
                "ToolCall detected in stream"
            );
            vec![
                StreamChunk::ToolCallStarted {
                    id: tool_id.clone(),
                    name: tool_call.function.name.to_string(),
                },
                StreamChunk::ToolCallInput {
                    id: tool_id,
                    arguments: tool_call.function.arguments_json(),
                },
            ]
        }
        MultiTurnStreamItem::ToolResult { tool_result } => {
            let content_text = streamed_tool_result_to_text(&tool_result);
            let call_id = tool_result.call.to_string();

            if tool_result.is_error || tool_result_looks_like_error(&content_text) {
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
            // rig's record of the turn: the prompt, each assistant tool-call
            // message, each tool-result message, the final text. The
            // frontends persist its tool round-trips behind the final text
            // entry (AGE-247). After the usage aggregate, before `Done`.
            // `map_item` is the single mapping path, so this one arm replaces
            // the two duplicated macro sites AGE-247 originally had.
            if !final_response.messages.is_empty() {
                chunks.push(StreamChunk::TurnMessages(final_response.messages));
            }
            chunks
        }
        _ => Vec::new(),
    }
}

/// Classify a provider's failure: a malformed reply, a turn cut off with no
/// answer, or a transport failure. The HTTP status and a budget refusal are
/// read first, by [`classify_streaming_error`].
fn classify_provider_failure(kind: ErrorKind, message: &str) -> StreamErrorKind {
    match kind {
        // A malformed tool call surfaces from rig as a JSON parse failure on
        // the provider's response, not an HTTP status.
        ErrorKind::Json => StreamErrorKind::MalformedToolCall,
        // rig ends a run whose last turn answered nothing *and* was cut off
        // (at the output cap, or by a content filter) with this error. It is
        // an empty completion, not a transport failure: re-running the turn
        // hits the same cap, so headless must stop rather than retry it five
        // times (AGE-936). The in-turn retry (`EmptyTurnRetry`) had its go.
        _ if message.contains(NO_ANSWER_CUT_OFF) => StreamErrorKind::EmptyCompletion,
        ErrorKind::Cancelled => StreamErrorKind::Other,
        _ => StreamErrorKind::Transport,
    }
}

/// What rig's error says when a run's last turn produced no answer and its
/// provider cut it off. This matches rig-core 0.46's error text
/// (`FinishReason::no_answer_message`) verbatim and must be revisited on
/// every rig bump: rig has no typed variant for it. The session test
/// `a_capped_turn_with_no_answer_stops_without_a_loop` drives the real error
/// through this, so a rig that rewords it fails there.
const NO_ANSWER_CUT_OFF: &str = "produced no answer and stopped with finish_reason=";

/// The cut-off error's text when the request filled the model's configured
/// window: rig's "raise max_tokens" would send the reader the wrong way.
/// It keeps rig's own phrase, so [`is_cut_off_without_answer`] (and the
/// follow-up it leads to) still holds (AGE-967).
pub const WINDOW_EXHAUSTED_ERROR: &str = "the model produced no answer and stopped with \
     finish_reason=Length: the context window is exhausted — the request left the reply less \
     than 1024 tokens, so raising max_tokens cannot help; the history has to shrink";

/// Whether `error` is a run rig ended because its last turn ran out of
/// output (`finish_reason=Length`) before it answered anything, after the
/// in-turn cut-off ladder (`EmptyTurnRetry`, AGE-967) was spent. A content
/// filter's cut-off is not one: continuing would only meet the filter again.
pub fn is_cut_off_without_answer(error: &StreamError) -> bool {
    error.kind == StreamErrorKind::EmptyCompletion
        && error
            .message
            .contains(&format!("{NO_ANSWER_CUT_OFF}Length"))
}

/// Appended to a turn's answer when the run ended on a model call the
/// provider cut off at its output cap (`finish_reason: length`) and the text
/// was accepted as the answer — the retry in `EmptyTurnRetry` was already
/// spent (AGE-936). Without it a worker's partial answer reaches its leader,
/// and a human, as a complete success. The cap is not named: the hosted
/// egress may lower a call's `max_tokens` on its way out, so chatty does not
/// know the number that applied.
pub const TRUNCATION_NOTE: &str =
    "\n\n[Note: this answer was cut off at the output limit and may be incomplete.]";

/// The typed `budget_exhausted` text for a provider's `402` refusal on a
/// spent budget (AGE-903), read from its status and body.
fn budget_refusal(err: &PromptError) -> Option<String> {
    endpoint_refusal(
        err.provider_response_status()?.as_u16(),
        err.provider_response_body()?,
    )
}

/// Classify the error that ends rig's stream (AGE-244 / D5) into a
/// [`StreamErrorKind`], using the HTTP status the provider returned when
/// there is one.
fn classify_streaming_error(err: &PromptError) -> StreamErrorKind {
    if budget_refusal(err).is_some() {
        return StreamErrorKind::BudgetExhausted;
    }
    if let Some(status) = err.provider_response_status() {
        return match status.as_u16() {
            401 | 403 => StreamErrorKind::Auth,
            429 => StreamErrorKind::RateLimited,
            code => StreamErrorKind::ProviderStatus(code),
        };
    }
    match err {
        PromptError::Provider(error) => {
            classify_provider_failure(rig_core::ErrorReport::from(error).kind, &error.to_string())
        }
        PromptError::Report(report) => classify_provider_failure(report.kind, &report.message),
        // A one-token tool-name mistake (AGE-497): worth a nudge, not an
        // outright stop, since rig's own error text already lists the
        // available/allowed tools the model can retry with.
        PromptError::UnknownToolCall { .. } => StreamErrorKind::UnknownToolCall,
        // Exhausted its turn budget, or was cancelled: neither is a
        // provider transport failure, and neither is worth a nudge.
        _ => StreamErrorKind::Other,
    }
}

/// Start one rig run of `agent` on `history` and `prompt` under `budget`,
/// with the hooks every run of `stream_prompt` carries.
fn start_run(
    agent: &rig_agent::agent::Agent,
    concurrency: usize,
    history: Vec<Message>,
    prompt: Message,
    budget: TurnBudget,
    cut_offs: &CutOffCalls,
) -> rig_agent::agent::StreamingResult {
    // A delegation whose worker never started ends the run (AGE-822), and
    // so does one that came back with a spent budget (AGE-903). A tool call
    // whose arguments never parsed ends it too, for `stream_prompt` to
    // answer (AGE-983).
    budget
        .apply(agent.prompt(prompt).history(history))
        .tool_concurrency(concurrency)
        .add_hook(StopOnWorkerStartFailure)
        .add_hook(StopOnBudgetExhausted)
        .add_hook(cut_offs.clone())
        .stream()
}

/// Map one line of the agent's stream — an item, or the transport `Err` that
/// ends it — into the chunks it produces, and whether the caller should stop
/// reading further items.
fn map_stream_result(
    item: Result<MultiTurnStreamItem, PromptError>,
    semantics: UsageSemantics,
) -> (Vec<StreamChunk>, bool) {
    match item {
        Ok(item) => (map_item(item, semantics), false),
        Err(e) => {
            let kind = classify_streaming_error(&e);
            (
                vec![StreamChunk::Error(StreamError::new(
                    kind,
                    describe_streaming_error(&e),
                ))],
                true,
            )
        }
    }
}

/// Whether rig ended the run because a provider's response stream closed
/// before its end-of-stream marker (`[DONE]`, a `finish_reason`, Ollama's
/// `done: true`).
fn is_truncated_stream(err: &PromptError) -> bool {
    match err {
        PromptError::Provider(ProviderError::Truncated) => true,
        PromptError::Report(report) => report.message == ProviderError::Truncated.to_string(),
        _ => false,
    }
}

/// The error text shown to the user: rig's own, except for a stream cut
/// short, whose wording names rig internals rather than what happened.
fn describe_streaming_error(err: &PromptError) -> String {
    if let Some(refusal) = budget_refusal(err) {
        return refusal;
    }
    let raw = err.to_string();
    if is_truncated_stream(err) {
        format!(
            "The connection to the model provider closed before the reply finished. \
             This is usually a network, VPN/proxy or provider-side interruption; try again. \
             ({raw})"
        )
    } else {
        raw
    }
}

/// Shown when a run's tool-free last call asked for a tool instead of
/// answering and produced no text of its own.
pub const BUDGET_SPENT_NOTE: &str = "[Stopped: this response's turn or time budget is spent and \
     tools were disabled for a final answer, but the model asked for another tool call instead \
     of answering.]";

/// The normal end of a run whose tool-free wrap-up call asked for a tool
/// anyway (see [`crate::services::turn_budget`]): rig reports it as
/// `MaxTurns` (a tool call left in the reasoning channel trips
/// `EmptyTurnRetry` into one call past the budget; "reached max turns
/// limit: 51" under `--max-agent-turns 50`) or as the budget hook's own
/// stop. Either way the model had its last word, so the run ends with it:
/// rig's record of this run's messages (past the `history_len` it started
/// with), a note when the last call gave no text, then `Done`. `None` for
/// every other error.
fn budget_spent_end(
    err: &PromptError,
    history_len: usize,
    text_in_last_call: bool,
) -> Option<Vec<StreamChunk>> {
    let history: &[Message] = match err {
        PromptError::MaxTurns { chat_history, .. } => chat_history,
        PromptError::Cancelled {
            chat_history,
            reason,
        } if reason == WRAP_UP_TOOL_CALL_STOP => chat_history,
        _ => return None,
    };
    warn!(error = %err, "Budget spent on a call that asked for a tool; ending the run normally");
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

/// The end of a run whose `invoke_agent` call could not start its worker
/// (see [`crate::services::worker_start`]): the run's messages so far, the
/// card the hook stopped it with as its text — after a blank line when the
/// last call already said something — then `Done`. A normal end, not an
/// error: the card is the answer, which the user reads and a sub-leader's
/// caller is handed. `None` for every other error.
fn worker_start_end(
    err: &PromptError,
    history_len: usize,
    text_in_last_call: bool,
) -> Option<Vec<StreamChunk>> {
    let PromptError::Cancelled {
        chat_history,
        reason,
    } = err
    else {
        return None;
    };
    let card = reason.strip_prefix(WORKER_START_STOP)?;
    let mut chunks = Vec::new();
    if let Some(messages) = chat_history.get(history_len..).filter(|m| !m.is_empty()) {
        chunks.push(StreamChunk::TurnMessages(messages.to_vec()));
    }
    let separator = if text_in_last_call { "\n\n" } else { "" };
    chunks.push(StreamChunk::Text(format!("{separator}{card}")));
    chunks.push(StreamChunk::Done);
    Some(chunks)
}

/// The end of a run whose `invoke_agent` call came back with a spent budget
/// (see [`crate::services::budget_stop`]): as [`worker_start_end`], with the
/// final message the hook wrote — no model call — as the run's text.
fn budget_stop_end(
    err: &PromptError,
    history_len: usize,
    text_in_last_call: bool,
) -> Option<Vec<StreamChunk>> {
    let PromptError::Cancelled {
        chat_history,
        reason,
    } = err
    else {
        return None;
    };
    let message = reason.strip_prefix(BUDGET_STOP)?;
    let mut chunks = Vec::new();
    if let Some(messages) = chat_history.get(history_len..).filter(|m| !m.is_empty()) {
        chunks.push(StreamChunk::TurnMessages(messages.to_vec()));
    }
    let separator = if text_in_last_call { "\n\n" } else { "" };
    chunks.push(StreamChunk::Text(format!("{separator}{message}")));
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
    err: &PromptError,
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

/// The text a dropped image is replaced with, so the model knows content
/// was removed instead of silently seeing fewer items than it sent/expects.
const IMAGE_UNSUPPORTED_NOTE: &str =
    "[Image omitted: the current model does not accept image input.]";

/// Strip image content from a request the configured model cannot accept,
/// replacing each dropped image with a text note so the model (and a human
/// reading the transcript) can tell content was removed rather than just
/// silently missing. Covers images in the new message, in prior turns'
/// history (e.g. after switching mid-conversation to a text-only model),
/// and inside tool results (e.g. `pdf_to_image`, a screenshot attached on a
/// later turn). A no-op, and free of any clone, when `supports_images` is
/// true.
fn strip_unsupported_images(
    history: Vec<Message>,
    contents: Vec<UserContent>,
    supports_images: bool,
) -> (Vec<Message>, Vec<UserContent>) {
    if supports_images {
        return (history, contents);
    }

    let strip_user_content = |items: Vec<UserContent>| -> Vec<UserContent> {
        items
            .into_iter()
            .map(|item| match item {
                UserContent::Image(_) => UserContent::Text(Text::new(IMAGE_UNSUPPORTED_NOTE)),
                UserContent::ToolResult(mut result) => {
                    result.content = result
                        .content
                        .into_iter()
                        .map(|c| match c {
                            ToolResultContent::Image(_) => {
                                ToolResultContent::Text(Text::new(IMAGE_UNSUPPORTED_NOTE))
                            }
                            other => other,
                        })
                        .collect();
                    UserContent::ToolResult(result)
                }
                other => other,
            })
            .collect()
    };

    let history = history
        .into_iter()
        .map(|message| match message {
            Message::User { content } => Message::User {
                content: strip_user_content(content),
            },
            other => other,
        })
        .collect();
    let contents = strip_user_content(contents);

    (history, contents)
}

/// How many of a leader's tool calls in one response run at once.
///
/// Only a delegating agent gets more than one: the calls it runs side by
/// side are `invoke_agent`s, each a worker in its own process, and the
/// broker's per-endpoint budget (ADR-0011 C6) still bounds what reaches the
/// model server. Any other agent's tools touch one workspace and one approval
/// queue, so they stay in call order.
const MAX_PARALLEL_DELEGATIONS: usize = 4;

fn tool_concurrency(agent: &AgentClient) -> usize {
    if agent.delegator().is_some() {
        MAX_PARALLEL_DELEGATIONS
    } else {
        1
    }
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
    // A model switch mid-conversation (history built for a vision model, now
    // sent to a text-only one) or an image-producing tool result would
    // otherwise reach a model configured as text-only as-is and come back as
    // a raw 400 (e.g. "At most 0 image(s) may be provided") instead of
    // something the model or the user can act on. Strip images up front and
    // tell the model instead. This trusts `ModelConfig::supports_images`: a
    // text-only model wrongly flagged `true` (OpenRouter's blanket default)
    // still sends its images, and a vision model flagged `false` loses them.
    let (history, contents) = strip_unsupported_images(history, contents, agent.supports_images());
    let user_message = Message::User { content: contents };
    let semantics = agent.provider().usage_semantics();
    let model = agent.model_ref();
    let context_shaper = agent.context_shaper().clone();
    let history_len = history.len();
    let request_recorder = agent.request_recorder().clone();
    request_recorder.clear();
    let plugin_usage = agent.plugin_usage().to_vec();

    // The turn budget sets rig's call cap (the tool turns plus one tool-free
    // wrap-up call) and tells the model how many tool turns it has left.
    // A leader that asks for several delegations in one response gets them
    // run side by side; every other agent keeps its tools in call order.
    // A budget stop belongs to the run it happened in: a new run may
    // delegate again, and is refused again if the budget is still spent.
    if let Some(delegator) = agent.delegator() {
        delegator.clear_budget_stop();
    }
    let rig_agent = agent.agent.clone();
    let concurrency = tool_concurrency(agent);
    let cut_offs = CutOffCalls::default();
    let mut agent_stream = start_run(
        &rig_agent,
        concurrency,
        history,
        user_message,
        turn_budget,
        &cut_offs,
    );

    // A caller that does not wire a channel is treated the same as one whose
    // sender already dropped: `recv()` resolves to `None` immediately, which
    // disables that `select!` arm for the rest of the stream.
    let mut approval_rx = approval_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);
    let mut resolution_rx = resolution_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);
    let mut clarification_rx = clarification_rx.unwrap_or_else(|| mpsc::unbounded_channel().1);

    let stream: ResponseStream = Box::pin(async_stream::stream! {
        // Whether the model call in flight has said anything yet.
        let mut text_in_last_call = false;
        // Whether the latest model call ended at the output cap (AGE-936).
        let mut last_call_cut_off = false;
        let mut clock = UsageClock::new(model);
        // A cut-off tool call answered in-run (AGE-983): how many this turn,
        // the model calls spent so far (for the continued run's budget), the
        // text and the arguments of the call in flight, and the messages of
        // the runs before the current one, which its final record lacks.
        let mut recoveries = 0;
        let mut calls = 0;
        let mut said = String::new();
        let mut arguments = ArgumentTail::default();
        let mut earlier_runs: Vec<Message> = Vec::new();
        loop {
            tokio::select! {
                item = agent_stream.next() => {
                    // A compaction runs in the hook ahead of a model call, so
                    // its summary call has finished by the time anything that
                    // call produces arrives (AGE-683).
                    for call in context_shaper.take_compaction_usage() {
                        clock.exclude(&call);
                        yield Ok(StreamChunk::CompactionUsage(call));
                    }
                    // A plugin's calls ran inside a tool, which has returned
                    // by the time anything after it arrives; the last ones
                    // are drained by the end of the stream (`None`).
                    for (plugin, usage) in &plugin_usage {
                        for call in usage.take() {
                            yield Ok(StreamChunk::PluginUsage { plugin: plugin.clone(), call });
                        }
                    }
                    match item {
                        Some(result) => {
                            // A cut-off call this turn can no longer answer
                            // ends it as a malformed tool call.
                            let mut cut_unanswered = false;
                            if result.is_err()
                                && let Some(cut) = cut_offs.take()
                            {
                                // A call whose arguments never parsed, which
                                // `CutOffCalls` failed the run on.
                                warn!(
                                    tool = %cut.tool,
                                    reason = %cut.reason,
                                    finish_reason = "not length: tool_calls, stop, unknown or none",
                                    argument_chars = arguments.chars(),
                                    argument_tail = %arguments.tail(),
                                    recovery = recoveries + 1,
                                    max = MAX_CUT_OFF_RECOVERIES,
                                    "Tool call cut off before its arguments were complete"
                                );
                                let sent = if recoveries < MAX_CUT_OFF_RECOVERIES {
                                    request_recorder.take()
                                } else {
                                    None
                                };
                                if let Some(sent) = sent {
                                    recoveries += 1;
                                    calls += 1;
                                    let id = format!("call_cut_off_{}", uuid::Uuid::new_v4().simple());
                                    let (call, result) = cut.messages(&id, &said);
                                    for chunk in [
                                        StreamChunk::ToolCallStarted { id: id.clone(), name: cut.tool.clone() },
                                        StreamChunk::ToolCallInput { id: id.clone(), arguments: "{}".to_string() },
                                        StreamChunk::ToolCallError { id, error: cut.tool_result() },
                                    ] {
                                        yield Ok(chunk);
                                    }
                                    text_in_last_call = false;
                                    last_call_cut_off = false;
                                    said.clear();
                                    arguments.clear();
                                    earlier_runs = sent.get(history_len..).unwrap_or_default().to_vec();
                                    earlier_runs.push(call.clone());
                                    let mut history = sent;
                                    history.push(call);
                                    let budget = turn_budget.continued(calls);
                                    agent_stream =
                                        start_run(&rig_agent, concurrency, history, result, budget, &cut_offs);
                                    continue;
                                }
                                cut_unanswered = true;
                            }
                            if let Err(e) = &result
                                && let Some(chunks) = budget_spent_end(e, history_len, text_in_last_call)
                                    .or_else(|| worker_start_end(e, history_len, text_in_last_call))
                                    .or_else(|| budget_stop_end(e, history_len, text_in_last_call))
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
                            // Whether the run's latest model call was cut
                            // off at the output cap; read when the run ends.
                            match &result {
                                Ok(MultiTurnStreamItem::CompletionCall(call)) => {
                                    calls += 1;
                                    last_call_cut_off = call.finish_reason
                                        == Some(rig_core::completion::FinishReason::Length);
                                }
                                // The arguments of the call in flight, for
                                // the log line should it be cut off.
                                Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(
                                    StreamEvent::Start { kind: PartKind::ToolCall, .. },
                                ))) => arguments.clear(),
                                Ok(MultiTurnStreamItem::StreamAssistantItem(Item::Event(
                                    StreamEvent::Arguments { json, .. },
                                ))) => arguments.push(json),
                                Ok(MultiTurnStreamItem::FinalResponse(_))
                                    if last_call_cut_off && text_in_last_call =>
                                {
                                    yield Ok(StreamChunk::Text(TRUNCATION_NOTE.to_string()));
                                }
                                _ => {}
                            }
                            let (chunks, stop) = map_stream_result(result, semantics);
                            for mut chunk in chunks {
                                clock.stamp(&mut chunk);
                                // A continued run's record starts at the
                                // cut-off call's result: the runs before it
                                // go first, so the turn is persisted whole.
                                if let StreamChunk::TurnMessages(messages) = &mut chunk
                                    && !earlier_runs.is_empty()
                                {
                                    messages.splice(0..0, earlier_runs.iter().cloned());
                                }
                                if let StreamChunk::Error(error) = &mut chunk
                                    && cut_unanswered
                                {
                                    error.kind = StreamErrorKind::MalformedToolCall;
                                }
                                if let StreamChunk::Error(error) = &mut chunk
                                    && is_cut_off_without_answer(error)
                                    && context_shaper.window_exhausted()
                                {
                                    error.message = WINDOW_EXHAUSTED_ERROR.to_string();
                                }
                                match &chunk {
                                    StreamChunk::Text(text) => {
                                        said.push_str(text);
                                        if !text.trim().is_empty() {
                                            text_in_last_call = true;
                                        }
                                    }
                                    // The next call starts after the results.
                                    StreamChunk::ToolCallResult { .. }
                                    | StreamChunk::ToolCallError { .. } => {
                                        text_in_last_call = false;
                                        said.clear();
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
                    yield Ok(approval_chunk(approval));
                }

                // Process resolution notifications
                Some(resolution) = resolution_rx.recv() => {
                    debug!(
                        id = %resolution.id,
                        approved = resolution.approved,
                        "Stream received resolution notification, emitting ApprovalResolved chunk"
                    );
                    for chunk in requests_before_resolution(&mut approval_rx, resolution) {
                        yield Ok(chunk);
                    }
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

fn approval_chunk(approval: ApprovalNotification) -> StreamChunk {
    StreamChunk::ApprovalRequested {
        id: approval.id,
        command: approval.command,
        is_sandboxed: approval.is_sandboxed,
        detail: approval.detail,
    }
}

/// The chunks for a resolution just received: every approval request already
/// queued first, then the resolution. The two channels are raced by a
/// non-biased `select!`, and a request answered at once (an unattended run,
/// AGE-917) would otherwise be resolved before it was shown, leaving its
/// block pending for good.
fn requests_before_resolution(
    approval_rx: &mut mpsc::UnboundedReceiver<ApprovalNotification>,
    resolution: ApprovalResolution,
) -> Vec<StreamChunk> {
    let mut chunks = Vec::new();
    while let Ok(approval) = approval_rx.try_recv() {
        chunks.push(approval_chunk(approval));
    }
    chunks.push(StreamChunk::ApprovalResolved {
        id: resolution.id,
        approved: resolution.approved,
    });
    chunks
}

#[cfg(test)]
mod tests {
    /// AGE-917: a request denied the moment it is raised puts its request
    /// and its resolution on the two channels back to back; whichever the
    /// stream's `select!` picks, the request must be emitted first.
    #[test]
    fn a_resolution_is_preceded_by_the_requests_already_queued() {
        use crate::models::execution_approval_store::{
            ApprovalDetail, ApprovalNotification, ApprovalResolution,
        };
        use crate::services::StreamChunk;

        let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel();
        approval_tx
            .send(ApprovalNotification {
                id: "a1".into(),
                command: "[shell] echo hi".into(),
                is_sandboxed: false,
                detail: ApprovalDetail::exec("[shell] echo hi"),
            })
            .unwrap();
        let chunks = super::requests_before_resolution(
            &mut approval_rx,
            ApprovalResolution {
                id: "a1".into(),
                approved: false,
            },
        );
        assert!(matches!(
            chunks.as_slice(),
            [
                StreamChunk::ApprovalRequested { id: requested, .. },
                StreamChunk::ApprovalResolved { id: resolved, approved: false },
            ] if requested == "a1" && resolved == "a1"
        ));
    }

    use rig_agent::agent::CompletionCall;
    use rig_core::ProviderResponseError;
    use rig_core::completion::message::{
        CallId, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    };
    use rig_core::error::ProviderError;
    use rig_core::streaming::{Item, Part, PartKind, StreamEvent};
    use std::sync::Arc;

    use super::{
        BUDGET_SPENT_NOTE, IMAGE_UNSUPPORTED_NOTE, Message, MultiTurnStreamItem, PromptError,
        RequestRecorder, StreamChunk, StreamErrorKind, UsageClock, UsageSemantics,
        WRAP_UP_TOOL_CALL_STOP, budget_spent_end, classify_streaming_error, failed_run_messages,
        map_item, map_stream_result, normalize_usage, streamed_tool_result_to_text,
        strip_unsupported_images, tool_result_looks_like_error,
    };

    /// The usage rig 0.46 reads off an OpenRouter chat completion whose
    /// `usage` block is `usage`, through chatty's own OpenRouter client.
    async fn openrouter_usage(usage: serde_json::Value) -> rig_core::completion::Usage {
        use crate::settings::models::models_store::ModelConfig;
        use crate::settings::models::providers_store::{ProviderConfig, ProviderType};
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "gen-1",
                "object": "chat.completion",
                "created": 0,
                "model": "m",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop",
                }],
                "usage": usage,
            })))
            .mount(&server)
            .await;
        let model = ModelConfig::new("m".into(), "m".into(), ProviderType::OpenRouter, "m".into());
        let provider = ProviderConfig::new("or".into(), ProviderType::OpenRouter)
            .with_api_key("k".into())
            .with_base_url(format!("{}/v1", server.uri()));
        crate::factories::agent_factory::completion_model(&model, &provider)
            .unwrap()
            .for_request(false)
            .call(rig_core::completion::CompletionRequest::new(Message::user(
                "hi",
            )))
            .await
            .expect("the fake server answers")
            .usage
    }

    fn status_error(code: u16) -> PromptError {
        PromptError::Provider(ProviderError::ProviderResponse(ProviderResponseError::new(
            reqwest::StatusCode::from_u16(code).unwrap(),
            "{}",
        )))
    }

    fn response_error(message: &str) -> PromptError {
        PromptError::Provider(ProviderError::Response(message.to_string()))
    }
    use rig_core::message::{Image, UserContent};

    // ── AGE image-input-on-text-only-model guard ─────────────────────────────

    #[test]
    fn supported_model_passes_images_through_unchanged() {
        let contents = vec![UserContent::Image(Image::default())];
        let history = vec![Message::User {
            content: vec![UserContent::Image(Image::default())],
        }];

        let (history, contents) = strip_unsupported_images(history, contents, true);

        assert!(matches!(contents[0], UserContent::Image(_)));
        let Message::User { content } = &history[0] else {
            panic!("expected a user message");
        };
        assert!(matches!(content[0], UserContent::Image(_)));
    }

    #[test]
    fn unsupported_model_replaces_a_new_message_image_with_a_note() {
        let contents = vec![UserContent::Image(Image::default())];

        let (_, contents) = strip_unsupported_images(Vec::new(), contents, false);

        match &contents[0] {
            UserContent::Text(text) => assert_eq!(text.text, IMAGE_UNSUPPORTED_NOTE),
            other => panic!("expected the image to become a text note, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_model_replaces_a_history_image_with_a_note() {
        let history = vec![Message::User {
            content: vec![UserContent::Image(Image::default())],
        }];

        let (history, _) = strip_unsupported_images(history, Vec::new(), false);

        let Message::User { content } = &history[0] else {
            panic!("expected a user message");
        };
        match &content[0] {
            UserContent::Text(text) => assert_eq!(text.text, IMAGE_UNSUPPORTED_NOTE),
            other => panic!("expected the image to become a text note, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_model_replaces_a_tool_result_image_with_a_note() {
        let tool_result = ToolResult {
            call: CallId::from_wire("call-1"),
            name: crate::testing::tool_name("pdf_to_image"),
            content: vec![ToolResultContent::Image(Image::default())],
            is_error: false,
        };
        let history = vec![Message::User {
            content: vec![UserContent::ToolResult(tool_result)],
        }];

        let (history, _) = strip_unsupported_images(history, Vec::new(), false);

        let Message::User { content } = &history[0] else {
            panic!("expected a user message");
        };
        let UserContent::ToolResult(result) = &content[0] else {
            panic!("expected a tool result");
        };
        match &result.content[0] {
            ToolResultContent::Text(text) => assert_eq!(text.text, IMAGE_UNSUPPORTED_NOTE),
            other => panic!("expected the image to become a text note, got {other:?}"),
        }
    }

    /// Anthropic reports `input_tokens` without the cached share; OpenAI-style
    /// usage reports it inside `prompt_tokens`. The same activity must yield
    /// the same normalised record either way, or the hit rate would mean
    /// something different per provider.
    #[test]
    fn usage_normalisation_agrees_across_provider_conventions() {
        let mut anthropic = rig_core::completion::Usage::new();
        anthropic.input_tokens = Some(200);
        anthropic.cached_input_tokens = Some(9_000);
        anthropic.cache_creation_input_tokens = Some(800);
        anthropic.output_tokens = Some(30);

        let mut openai_style = rig_core::completion::Usage::new();
        openai_style.input_tokens = Some(200 + 9_000 + 800);
        openai_style.cached_input_tokens = Some(9_000);
        openai_style.cache_creation_input_tokens = Some(800);
        openai_style.output_tokens = Some(30);

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
    #[tokio::test]
    async fn openrouter_cached_tokens_reach_the_normalised_record() {
        let reported = openrouter_usage(
            serde_json::from_str(
                r#"{"prompt_tokens":5000,"completion_tokens":120,"total_tokens":5120,
                "prompt_tokens_details":{"cached_tokens":1200,"cache_write_tokens":300}}"#,
            )
            .unwrap(),
        )
        .await;
        let call = normalize_usage(UsageSemantics::InputIncludesCache, 1, &reported);
        assert_eq!(call.cache_read_tokens, 1_200);
        assert_eq!(call.cache_write_tokens, 300);
        // OpenRouter counts both inside `prompt_tokens`; chatty stores the
        // uncached share, so the three still add back to the whole prompt.
        assert_eq!(call.input_tokens, 3_500);
        assert_eq!(call.prompt_tokens(), 5_000);

        let uncached = openrouter_usage(
            serde_json::from_str(
                r#"{"prompt_tokens":5000,"completion_tokens":120,"total_tokens":5120}"#,
            )
            .unwrap(),
        )
        .await;
        let call = normalize_usage(UsageSemantics::InputIncludesCache, 1, &uncached);
        assert_eq!(call.cache_read_tokens, 0);
        assert_eq!(call.input_tokens, 5_000);
        assert_eq!(call.cache_hit_rate(), Some(0.0));
    }

    /// Reasoning tokens reach the per-call record, where the headless usage
    /// file sums them (they are part of `output_tokens`, not on top of it).
    #[tokio::test]
    async fn reasoning_tokens_reach_the_normalised_record() {
        let reported = openrouter_usage(
            serde_json::from_str(
                r#"{"prompt_tokens":50,"completion_tokens":40,"total_tokens":90,
                "completion_tokens_details":{"reasoning_tokens":25}}"#,
            )
            .unwrap(),
        )
        .await;
        let call = normalize_usage(UsageSemantics::InputIncludesCache, 1, &reported);
        assert_eq!(call.reasoning_tokens, 25);
        assert_eq!(call.output_tokens, 40);
    }

    #[test]
    fn usage_normalisation_never_underflows_on_inconsistent_counts() {
        let mut odd = rig_core::completion::Usage::new();
        odd.input_tokens = Some(10);
        odd.cached_input_tokens = Some(50);
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
            call: CallId::from_wire("call-1"),
            name: crate::testing::tool_name("query_data"),
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
            is_error: false,
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
        let item = MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
            part: Part::new(0),
            text: "hello".into(),
        }));
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
        let item = MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Reasoning {
            part: Part::new(0),
            text: "Let me think".into(),
        }));
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
        for event in [
            StreamEvent::Start {
                part: Part::new(0),
                kind: PartKind::ToolCall,
                name: Some(crate::testing::tool_name("write_file")),
            },
            StreamEvent::Arguments {
                part: Part::new(0),
                json: "{\"path\": \"src/".into(),
            },
        ] {
            let item = MultiTurnStreamItem::StreamAssistantItem(Item::Event(event));
            let chunks = map_item(item, UsageSemantics::InputIncludesCache);
            assert_eq!(chunks.len(), 1);
            assert!(matches!(&chunks[0], StreamChunk::ToolCallDelta));
        }
    }

    /// rig 0.46 reports a call once history holds it (a projection of
    /// committed history, AGE-1151); its one id is the provider's.
    #[test]
    fn map_item_tool_call_uses_the_provider_call_id() {
        let tool_call = ToolCall::from_wire(
            "call-99",
            ToolFunction::new(
                crate::testing::tool_name("read_file"),
                serde_json::json!({"path": "README.md"}),
            ),
        );
        let item = MultiTurnStreamItem::ToolCall { tool_call };
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
    fn map_item_tool_call_without_a_provider_id_uses_rigs_own() {
        let local = rig_core::message::LocalCallId::new();
        let tool_call = ToolCall::new(
            CallId::Local(local),
            ToolFunction::new(
                crate::testing::tool_name("search_code"),
                serde_json::json!({}),
            ),
        );
        let item = MultiTurnStreamItem::ToolCall { tool_call };
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert!(matches!(
            &chunks[0],
            StreamChunk::ToolCallStarted { id, .. } if *id == local.to_string()
        ));
    }

    #[test]
    fn map_item_tool_result_text_is_success() {
        let tool_result = ToolResult {
            call: CallId::from_wire("call-1"),
            name: crate::testing::tool_name("read_file"),
            content: vec![ToolResultContent::text("# Chatty")],
            is_error: false,
        };
        let item = MultiTurnStreamItem::ToolResult { tool_result };
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
            call: CallId::from_wire("call-1"),
            name: crate::testing::tool_name("run_shell"),
            content: vec![ToolResultContent::text(
                "Error: run_shell: exited with status 1",
            )],
            is_error: false,
        };
        let item = MultiTurnStreamItem::ToolResult { tool_result };
        let chunks = map_item(item, UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(
            &chunks[0],
            StreamChunk::ToolCallError { id, error }
                if id == "call-1" && error.starts_with("Error:")
        ));
    }

    #[test]
    fn map_item_tool_result_json_is_serialised_to_text() {
        let tool_result = ToolResult {
            call: CallId::from_wire("call-1"),
            name: crate::testing::tool_name("query_data"),
            content: vec![ToolResultContent::json(serde_json::json!({"rows": 1}))],
            is_error: false,
        };
        let item = MultiTurnStreamItem::ToolResult { tool_result };
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
            serde_json::Value::Null,
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
        usage.input_tokens = Some(100);
        usage.output_tokens = Some(20);
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
        let err = response_error("boom");
        let (chunks, stop) = map_stream_result(Err(err), UsageSemantics::InputIncludesCache);
        assert_eq!(chunks.len(), 1);
        assert!(matches!(
            &chunks[0],
            StreamChunk::Error(e) if e.message.contains("boom") && e.kind == StreamErrorKind::Transport
        ));
        assert!(stop, "a transport error must stop the stream");
    }

    #[test]
    fn a_truncated_stream_says_the_connection_closed() {
        let err = PromptError::Provider(ProviderError::Truncated);
        let (chunks, _) = map_stream_result(Err(err), UsageSemantics::InputIncludesCache);
        assert!(matches!(
            &chunks[0],
            StreamChunk::Error(e) if e.kind == StreamErrorKind::Transport
                && e.message.starts_with("The connection to the model provider closed")
                && e.message.contains("the reply ended before the provider ended it")
        ));
    }

    // -------------------------------------------------------------------
    // Error classification (AGE-244 / D5): one rig error per kind maps
    // correctly to a StreamErrorKind.
    // -------------------------------------------------------------------

    #[test]
    fn classifies_401_as_auth() {
        assert_eq!(
            classify_streaming_error(&status_error(401)),
            StreamErrorKind::Auth
        );
    }

    #[test]
    fn classifies_403_as_auth() {
        assert_eq!(
            classify_streaming_error(&status_error(403)),
            StreamErrorKind::Auth
        );
    }

    #[test]
    fn classifies_429_as_rate_limited() {
        assert_eq!(
            classify_streaming_error(&status_error(429)),
            StreamErrorKind::RateLimited
        );
    }

    #[test]
    fn classifies_5xx_as_provider_status() {
        assert_eq!(
            classify_streaming_error(&status_error(503)),
            StreamErrorKind::ProviderStatus(503)
        );
    }

    #[test]
    fn classifies_json_error_as_malformed_tool_call() {
        let json_err = serde_json::from_str::<serde_json::Value>("{not json").unwrap_err();
        assert_eq!(
            classify_streaming_error(&PromptError::Provider(ProviderError::Json(Arc::new(
                json_err
            )))),
            StreamErrorKind::MalformedToolCall
        );
    }

    #[test]
    fn classifies_response_error_with_no_status_as_transport() {
        assert_eq!(
            classify_streaming_error(&response_error("boom")),
            StreamErrorKind::Transport
        );
    }

    /// AGE-936: a run rig ended because its last turn was cut off with no
    /// answer is an empty completion, which every surface stops on, not a
    /// transport failure headless would re-run five times into the same cap.
    #[test]
    fn classifies_a_cut_off_turn_with_no_answer_as_empty() {
        let err = response_error(&rig_core::completion::FinishReason::Length.no_answer_message());
        assert_eq!(
            classify_streaming_error(&err),
            StreamErrorKind::EmptyCompletion
        );
        assert_eq!(
            crate::services::decide_recovery(
                StreamErrorKind::EmptyCompletion,
                crate::services::StreamSurface::Headless,
                0
            ),
            crate::services::RecoveryAction::Stop
        );
    }

    #[test]
    fn classifies_max_turns_as_other() {
        let err = PromptError::MaxTurns {
            max_turns: 10,
            chat_history: Vec::new(),
            prompt: Message::user("hi"),
        };
        assert_eq!(classify_streaming_error(&err), StreamErrorKind::Other);
    }

    /// AGE-497: a hallucinated tool name is worth a nudge, not the same
    /// outright `Stop` as max-turns/cancellation/memory errors.
    #[test]
    fn classifies_unknown_tool_call_distinctly_from_other() {
        let err = PromptError::UnknownToolCall {
            tool_name: "web_search".to_string(),
            available_tools: vec!["search_web".to_string()],
            allowed_tools: vec!["search_web".to_string()],
            chat_history: Vec::new(),
        };
        assert_eq!(
            classify_streaming_error(&err),
            StreamErrorKind::UnknownToolCall
        );
    }

    /// A provider failure relayed as an effect report keeps its status.
    #[test]
    fn classifies_a_relayed_report_by_its_status() {
        let report = rig_core::ErrorReport::from(ProviderError::ProviderResponse(
            ProviderResponseError::new(reqwest::StatusCode::UNAUTHORIZED, "{}"),
        ));
        let err = PromptError::Report(report);
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
        let max_turns = PromptError::MaxTurns {
            max_turns: 51,
            chat_history: history.clone(),
            prompt: Message::user("tool results"),
        };
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

        let stopped = PromptError::Cancelled {
            chat_history: history,
            reason: WRAP_UP_TOOL_CALL_STOP.to_string(),
        };
        assert_eq!(
            chunk_names(&budget_spent_end(&stopped, 4, true).unwrap()),
            vec!["Done"]
        );
    }

    /// Any other error, a user's cancel included, is still an error.
    #[test]
    fn other_errors_are_not_budget_ends() {
        let cancelled = PromptError::Cancelled {
            chat_history: Vec::new(),
            reason: "user stop".to_string(),
        };
        assert!(budget_spent_end(&cancelled, 0, true).is_none());
        let transport = PromptError::Provider(ProviderError::Provider("boom".into()));
        assert!(budget_spent_end(&transport, 0, true).is_none());
    }

    /// A run that failed on the wire hands on the round-trips its last
    /// request carried, past the history it started with; an error that
    /// ends a run on purpose does not.
    #[test]
    fn a_failed_run_hands_on_what_its_last_call_was_sending() {
        let earlier = Message::user("an earlier turn");
        let prompt = Message::user("the task");
        let call = Message::Assistant(rig_core::message::AssistantMessage::new(vec![
            rig_core::message::AssistantContent::ToolCall(ToolCall::new(
                CallId::from_wire("call_1"),
                ToolFunction::new(
                    crate::testing::tool_name("read_file"),
                    serde_json::json!({"path": "a"}),
                ),
            )),
        ]));
        let result = Message::User {
            content: vec![rig_core::message::UserContent::ToolResult(ToolResult {
                call: CallId::from_wire("call_1"),
                name: crate::testing::tool_name("read_file"),
                content: vec![ToolResultContent::text("contents")],
                is_error: false,
            })],
        };
        let recorder = RequestRecorder::default();
        let transport = PromptError::Provider(ProviderError::Provider(
            "Http client error: error sending request for url".into(),
        ));

        recorder.record(
            [&earlier, &prompt, &call, &result]
                .map(|m| Arc::new(m.clone()))
                .to_vec(),
        );
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

        recorder.record([earlier, prompt, call, result].map(Arc::new).to_vec());
        let cancelled = PromptError::Cancelled {
            chat_history: Vec::new(),
            reason: "stop".into(),
        };
        assert!(failed_run_messages(&cancelled, &recorder, 1).is_none());
    }

    /// AGE-683: a compaction's summary call is timed on its own record, so
    /// the model call whose window it ran in is not charged for it too.
    #[test]
    fn a_compaction_is_not_counted_inside_the_next_call() {
        use crate::models::token_usage::{ApiCallUsage, ModelRef};
        use crate::settings::models::providers_store::ProviderType;
        use std::time::{Duration, Instant};

        let mut clock = UsageClock::new(ModelRef {
            provider: ProviderType::Ollama,
            model_id: "m".into(),
        });
        clock.call_started = Instant::now() - Duration::from_millis(200);
        clock.exclude(&ApiCallUsage {
            duration_ms: 150,
            ..ApiCallUsage::default()
        });

        let mut chunk = StreamChunk::ApiCallUsage(ApiCallUsage {
            turn: 1,
            ..ApiCallUsage::default()
        });
        clock.stamp(&mut chunk);
        let StreamChunk::ApiCallUsage(call) = chunk else {
            unreachable!()
        };
        assert!(
            (50..150).contains(&call.duration_ms),
            "the model call keeps only its own ~50ms: {}",
            call.duration_ms
        );
    }
}
