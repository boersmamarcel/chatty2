//! The in-loop context guard (AGE-504).
//!
//! Every chat agent carries a [`ContextShaper`] as a rig [`AgentHook`]. Before
//! each model call of a run — the first one and every call after a tool
//! round-trip — the hook measures the history the call would send and, only
//! when the request would not fit the model's context window, hands rig a
//! shorter history for that one request through
//! [`RequestPatch::history`]. Nothing is persisted: the conversation keeps
//! every message, and a request that fits is sent byte for byte as recorded,
//! so the append-only prefix property (AGE-277) is untouched for every
//! conversation that fits.
//!
//! This is a crash guard, not the compaction engine. It never calls a model;
//! the generational, persisted, LLM-summarised compaction is AGE-248's.
//!
//! # Budget
//!
//! The history budget in tokens is
//!
//! ```text
//! context_window − headroom − response_reserve − base − prompt
//! ```
//!
//! where `context_window` is the model's `max_context_window` (an assumed
//! 32k when the model has none configured), `headroom` is a tenth of it for
//! what the count cannot see (the chat template's per-message wrapping, and
//! the gap between the tiktoken estimate and the model's own tokenizer —
//! measured at ~2k tokens on a 32k Qwen request), `response_reserve` is the
//! model's `max_tokens` (4096 when unset), `base` is the preamble plus the
//! tool schemas — measured once when the agent is built, see
//! [`ContextShaper::set_base_tokens`] — and `prompt` is the message the call
//! is about to send, which is never shaped. Tokens are counted with the
//! model's [`TokenCounter`]; a typed tool's JSON result counts by its
//! serialized text, which is what the provider bills.
//!
//! # Stages
//!
//! Applied in order, cheapest information loss first, stopping as soon as the
//! history fits. The most recent `keep_tail` messages are what the model is
//! working with right now and stay whole for as long as possible.
//!
//! | # | Stage | What it does |
//! |---|-------|--------------|
//! | 1 | Cap | Stub every tool result over `tool_result_cap_bytes` outside the tail |
//! | 2 | Compact | Replace every tool result outside the tail with a one-liner |
//! | 3 | Snip | Keep the first `keep_head` and last `keep_tail` messages, drop the middle |
//! | 4 | Cap the tail | Stub, then one-line, tool results inside the tail, oldest first, never the last message |
//!
//! Stage 3's cut steps off a tool round-trip before it fires: the kept tail
//! starts no later than the assistant message issuing the calls it answers,
//! and the kept head ends no later than the last call the snip still answers.
//! Dropping half a round-trip is a 400 from every OpenAI-compatible provider,
//! and since the guard runs inside a live tool loop that is the cut's usual
//! shape, not an edge case (AGE-512).
//!
//! A history that is still over budget after stage 4 is sent as is: without
//! summarising there is nothing left to take.
//!
//! # The one result that can never fit
//!
//! The message a call is about to send — the latest tool results — is never
//! shaped, and a single result can be larger than the whole budget (a 36 KB
//! dump of 450 Wikipedia revisions was, in AGE-500). The only place that can
//! be caught is where the result is recorded: `on_tool_result` truncates a
//! result over two fifths of the history budget, once, so the same bytes go out on
//! every later call (the AGE-277 property holds for it too) and the model
//! sees as much of it as the window allows. Everything under that cap is
//! recorded whole.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, RequestPatch,
    ToolResultAction, ToolResultEvent,
};
use rig_core::completion::Message;
use rig_core::completion::message::{Text, ToolCallId, ToolResult, ToolResultContent};
use rig_core::message::UserContent;
use rig_core::tool::ToolOutput;
use tracing::{debug, warn};

use crate::services::{call_ids, enforce_tool_round_trips, result_ids, tool_round_trips_intact};
use crate::settings::models::models_store::ModelConfig;
use crate::token_budget::counter::TokenCounter;

// ── Tunables ──────────────────────────────────────────────────────────────────

/// Tool result payloads larger than this are stubbed by the cap stages.
const TOOL_RESULT_CAP_BYTES: usize = 8_192;

/// Messages kept whole at the head of the history when snipping.
const KEEP_HEAD: usize = 2;

/// Messages kept whole at the tail of the history: what the model is working
/// with right now.
const KEEP_TAIL: usize = 8;

/// The context window assumed for a model that has none configured. Every
/// local model chatty is run against today fits in 32k; a larger window only
/// costs a model some history it could have kept.
const ASSUMED_CONTEXT_WINDOW: usize = 32_768;

/// Tokens left for the model's reply when it has no `max_tokens` configured.
const RESPONSE_RESERVE: usize = 4_096;

/// Fraction of the window kept free for what the count cannot see: the chat
/// template's wrapping and the tokenizer mismatch (see the module docs).
const HEADROOM_FRACTION: f64 = 0.10;

/// The compact stages keep this many characters of a tool result.
const COMPACT_PREVIEW_CHARS: usize = 120;

/// The recording cap never drops below this many tokens, however small the
/// budget: a model whose base alone fills the window is broken anyway, and a
/// result cut to nothing would hide why.
pub const RECORDING_CAP_FLOOR_TOKENS: usize = 512;

// ── Public API ────────────────────────────────────────────────────────────────

/// Which stage was the last one applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextShaperStage {
    Cap,
    Compact,
    Snip,
    CapTail,
}

/// Settings that control the shaping pipeline.
///
/// All fields have sensible defaults. You can override individual values by
/// constructing the struct directly.
#[derive(Debug, Clone)]
pub struct ContextShaperSettings {
    /// Maximum bytes per tool result before a cap stage stubs it.
    pub tool_result_cap_bytes: usize,

    /// Messages kept whole at the head of the history when snipping.
    pub keep_head: usize,

    /// Messages kept whole at the tail of the history.
    pub keep_tail: usize,

    /// Context window assumed when the model has none configured.
    pub assumed_context_window: usize,

    /// Tokens reserved for the model's reply.
    pub response_reserve: usize,

    /// Fraction of the window kept free for the chat template and the
    /// tokenizer mismatch.
    pub headroom_fraction: f64,
}

impl Default for ContextShaperSettings {
    fn default() -> Self {
        Self {
            tool_result_cap_bytes: TOOL_RESULT_CAP_BYTES,
            keep_head: KEEP_HEAD,
            keep_tail: KEEP_TAIL,
            assumed_context_window: ASSUMED_CONTEXT_WINDOW,
            response_reserve: RESPONSE_RESERVE,
            headroom_fraction: HEADROOM_FRACTION,
        }
    }
}

/// Result of a shaping pass that changed the history.
#[derive(Debug, Clone)]
pub struct ShapedContext {
    /// The shortened history to send for this request.
    pub messages: Vec<Message>,

    /// The last stage applied.
    pub stage_applied: ContextShaperStage,

    /// Tokens the history counted before shaping.
    pub tokens_before: usize,

    /// Tokens the shaped history counts.
    pub tokens_after: usize,
}

/// The guard itself: one per agent, shared between the hook rig calls and
/// the factory that calibrates it after the agent is built.
#[derive(Clone)]
pub struct ContextShaper {
    inner: Arc<Inner>,
}

struct Inner {
    settings: ContextShaperSettings,
    counter: TokenCounter,
    context_window: Option<usize>,
    /// Preamble plus tool schemas, in tokens. Zero until the factory measures
    /// it, which only makes the guard more lenient, never wrong in the other
    /// direction.
    base_tokens: AtomicUsize,
}

impl ContextShaper {
    /// A guard for `model_config`: its `max_context_window` when it has one,
    /// its `max_tokens` as the reply reserve when set, its tokenizer.
    pub fn for_model(model_config: &ModelConfig) -> Self {
        let mut settings = ContextShaperSettings::default();
        if let Some(max_tokens) = model_config.max_tokens.filter(|n| *n > 0) {
            settings.response_reserve = max_tokens as usize;
        }
        let context_window = model_config
            .max_context_window
            .filter(|n| *n > 0)
            .map(|n| n as usize);
        Self::new(
            settings,
            TokenCounter::for_model(&model_config.model_identifier),
            context_window,
        )
    }

    /// A guard with explicit settings, counter and window (tests, and any
    /// caller that already knows the numbers).
    pub fn new(
        settings: ContextShaperSettings,
        counter: TokenCounter,
        context_window: Option<usize>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                settings,
                counter,
                context_window,
                base_tokens: AtomicUsize::new(0),
            }),
        }
    }

    /// Record what every request carries before any history: the preamble
    /// and the tool schemas, in tokens. Called once by the agent factory.
    pub fn set_base_tokens(&self, tokens: usize) {
        self.inner.base_tokens.store(tokens, Ordering::Relaxed);
    }

    /// The context window the guard shapes for.
    pub fn context_window(&self) -> usize {
        self.inner
            .context_window
            .unwrap_or(self.inner.settings.assumed_context_window)
    }

    /// Tokens available to the history once the headroom, the reply reserve,
    /// the base and `prompt_tokens` are taken off the window.
    pub fn history_budget(&self, prompt_tokens: usize) -> usize {
        let window = self.context_window();
        let headroom = (window as f64 * self.inner.settings.headroom_fraction) as usize;
        window
            .saturating_sub(headroom)
            .saturating_sub(self.inner.settings.response_reserve)
            .saturating_sub(self.inner.base_tokens.load(Ordering::Relaxed))
            .saturating_sub(prompt_tokens)
    }

    /// The counter this guard measures with.
    pub fn counter(&self) -> &TokenCounter {
        &self.inner.counter
    }

    /// Tokens a single tool result may occupy when it is recorded: two
    /// fifths of the history budget, so the newest two results fit together
    /// with a fifth to spare for the calls around them, with a floor of
    /// [`RECORDING_CAP_FLOOR_TOKENS`].
    pub fn recording_cap(&self) -> usize {
        (self.history_budget(0) * 2 / 5).max(RECORDING_CAP_FLOOR_TOKENS)
    }

    /// The presentation a tool result is recorded with: `output` itself when
    /// it fits the recording cap, else its text cut down to the cap with a
    /// header saying how much was kept. Image blocks are never touched.
    pub fn record_tool_output(&self, output: &ToolOutput) -> Option<ToolOutput> {
        let content = output.as_content();
        if content
            .iter()
            .any(|block| matches!(block, ToolResultContent::Image(_)))
        {
            return None;
        }
        let text = content
            .iter()
            .map(|block| match block {
                ToolResultContent::Text(text) => text.text.clone(),
                ToolResultContent::Json { value } => value.to_string(),
                ToolResultContent::Image(_) => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let tokens = self.inner.counter.count(&text);
        let cap = self.recording_cap();
        if tokens <= cap {
            return None;
        }
        // Keep the share of the text the cap allows, at this result's own
        // chars-per-token ratio, on a char boundary; the ratio of a prefix
        // is only close to the whole's, so recount and cut again while the
        // kept part plus its header is still over the cap.
        let header = |kept: usize| {
            format!(
                "[tool result truncated — kept {kept} of {} chars; the rest did not fit the model's context window]\n",
                text.len()
            )
        };
        let budget = cap.saturating_sub(self.inner.counter.count(&header(text.len())));
        let mut keep = text.len() * budget / tokens;
        for _ in 0..4 {
            let cut = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|i| *i <= keep)
                .last()
                .unwrap_or(0);
            let kept = &text[..cut];
            let kept_tokens = self.inner.counter.count(kept);
            if kept_tokens <= budget || cut == 0 {
                return Some(ToolOutput::text(format!("{}{kept}", header(cut))));
            }
            keep = cut * budget / kept_tokens;
        }
        Some(ToolOutput::text(header(0)))
    }

    /// The history one request should carry, or `None` when the recorded
    /// history goes out byte for byte — under budget and well formed, which
    /// is what keeps the append-only prefix property (AGE-277) and with it
    /// the provider's prompt cache.
    ///
    /// This is what the completion-call hook decides; it is a method so it
    /// can be tested without a `HookContext`, which rig only builds inside a
    /// run. `turn` is the model-call index, for the log line.
    pub fn request_history(
        &self,
        history: &[Message],
        prompt: &Message,
        turn: usize,
    ) -> Option<Vec<Message>> {
        let prompt_tokens = self.inner.counter.count_message(prompt);
        // rig hands the hook the prompt separately from the history, and
        // mid-tool-loop that prompt is the result answering the history's
        // last call — a call it answers is not a dangling one (AGE-513).
        let answered_by_prompt: Vec<ToolCallId> = result_ids(prompt).cloned().collect();

        let history = match self.shape(history, prompt_tokens) {
            Some(shaped) => {
                debug!(
                    turn,
                    stage = ?shaped.stage_applied,
                    tokens_before = shaped.tokens_before,
                    tokens_after = shaped.tokens_after,
                    budget = self.history_budget(prompt_tokens),
                    "context shaper applied before model call"
                );
                shaped.messages
            }
            None if tool_round_trips_intact(history, &answered_by_prompt) => return None,
            None => history.to_vec(),
        };

        // The last place we own before the request leaves. Shaping cannot
        // split a round-trip any more (AGE-512), but a history persisted
        // malformed by something else still would, and this is where that
        // stops being the provider's problem.
        Some(enforce_tool_round_trips(history, &answered_by_prompt))
    }

    /// Shape `history` for a request whose prompt costs `prompt_tokens`.
    /// `None` when it already fits: the request goes out exactly as recorded.
    pub fn shape(&self, history: &[Message], prompt_tokens: usize) -> Option<ShapedContext> {
        let settings = &self.inner.settings;
        let counter = &self.inner.counter;
        let budget = self.history_budget(prompt_tokens);

        let mut messages = history.to_vec();
        let mut counts: Vec<usize> = messages.iter().map(|m| counter.count_message(m)).collect();
        let tokens_before: usize = counts.iter().sum();
        if tokens_before <= budget {
            return None;
        }

        let len = messages.len();
        let tail_start = len.saturating_sub(settings.keep_tail);
        let fits = |counts: &[usize]| counts.iter().sum::<usize>() <= budget;
        let mut stage = ContextShaperStage::Cap;

        // Stage 1: cap oversized tool results outside the tail.
        for i in 0..tail_start {
            if cap_tool_results(&mut messages[i], settings.tool_result_cap_bytes) {
                counts[i] = counter.count_message(&messages[i]);
            }
        }

        // Stage 2: one-line every tool result outside the tail.
        if !fits(&counts) {
            stage = ContextShaperStage::Compact;
            for i in 0..tail_start {
                if compact_tool_results(&mut messages[i]) {
                    counts[i] = counter.count_message(&messages[i]);
                }
            }
        }

        // Stage 3: drop the middle. Both ends of the cut step off a tool
        // round-trip first, so neither half of one is ever dropped without
        // the other (AGE-512).
        if !fits(&counts) && len > settings.keep_head + settings.keep_tail + 1 {
            let head = round_trip_safe_head(&messages, settings.keep_head);
            let snip_end = round_trip_safe_tail_start(&messages, head, tail_start);
            if snip_end > head {
                stage = ContextShaperStage::Snip;
                let dropped = snip_end - head;
                let marker = Message::User {
                    content: vec![UserContent::Text(Text::new(format!(
                        "[CONTEXT SHAPER: {dropped} messages snipped to reduce context size]"
                    )))],
                };
                let marker_tokens = counter.count_message(&marker);
                let tail = messages.split_off(snip_end);
                let tail_counts = counts.split_off(snip_end);
                messages.truncate(head);
                counts.truncate(head);
                messages.push(marker);
                counts.push(marker_tokens);
                messages.extend(tail);
                counts.extend(tail_counts);
            }
        }

        // Stage 4: the tail itself, oldest first, never the message the
        // model is answering.
        if !fits(&counts) {
            stage = ContextShaperStage::CapTail;
            let len = messages.len();
            let tail_start = len.saturating_sub(settings.keep_tail);
            let last = len.saturating_sub(1);
            let cap = settings.tool_result_cap_bytes;
            let transforms: [&dyn Fn(&mut Message) -> bool; 2] =
                [&|m| cap_tool_results(m, cap), &compact_tool_results];
            'tail: for transform in transforms {
                for i in tail_start..last {
                    if transform(&mut messages[i]) {
                        counts[i] = counter.count_message(&messages[i]);
                        if fits(&counts) {
                            break 'tail;
                        }
                    }
                }
            }
        }

        let tokens_after = counts.iter().sum();
        if tokens_after > budget {
            warn!(
                tokens = tokens_after,
                budget,
                "context shaper: history is still over budget after every stage; sending as is"
            );
        }
        Some(ShapedContext {
            messages,
            stage_applied: stage,
            tokens_before,
            tokens_after,
        })
    }
}

impl AgentHook for ContextShaper {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        match self.request_history(event.history, event.prompt, event.turn) {
            Some(history) => CompletionCallAction::patch(RequestPatch::new().history(history)),
            None => CompletionCallAction::Continue,
        }
    }

    async fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> ToolResultAction {
        match self.record_tool_output(event.presentation) {
            Some(truncated) => {
                warn!(
                    tool = event.tool_name,
                    cap_tokens = self.recording_cap(),
                    "context shaper: tool result larger than the recording cap; recorded truncated"
                );
                ToolResultAction::Rewrite(truncated)
            }
            None => ToolResultAction::Keep,
        }
    }
}

// ── Round trips ───────────────────────────────────────────────────────────────

/// Pull the head of the snip back off an assistant `tool_calls` message whose
/// results the snip would drop: a call nothing answers is a 400 from every
/// OpenAI-compatible provider, the same one AGE-485 repaired at the other end.
fn round_trip_safe_head(messages: &[Message], mut head: usize) -> usize {
    while head > 0 {
        let answered: HashSet<&ToolCallId> = messages[..head].iter().flat_map(result_ids).collect();
        if messages[..head]
            .iter()
            .flat_map(call_ids)
            .all(|id| answered.contains(id))
        {
            break;
        }
        head -= 1;
    }
    head
}

/// Pull the start of the kept tail back to the assistant message that issued
/// the calls the tail answers. A tool result whose `tool_calls` message was
/// snipped away is rejected with "messages with role 'tool' must be a
/// response to a preceding message with 'tool_calls'" (AGE-512) — and since
/// the guard runs on every model call of a run, the cut lands inside a live
/// tool loop, where that shape is the common one rather than the rare one.
///
/// A result whose call is nowhere in the history is left where it is: that
/// history was already malformed, and moving the cut cannot repair it.
fn round_trip_safe_tail_start(messages: &[Message], head: usize, tail_start: usize) -> usize {
    let mut start = tail_start;
    while start > head {
        let called: HashSet<&ToolCallId> = messages[..head]
            .iter()
            .chain(&messages[start..])
            .flat_map(call_ids)
            .collect();
        let orphan = messages[start..]
            .iter()
            .flat_map(result_ids)
            .find(|id| !called.contains(*id))
            .cloned();
        let Some(orphan) = orphan else { break };
        let Some(call_index) = messages[head..start]
            .iter()
            .rposition(|message| call_ids(message).any(|id| *id == orphan))
            .map(|index| index + head)
        else {
            break;
        };
        start = call_index;
    }
    start
}

// ── Transforms ────────────────────────────────────────────────────────────────

/// Stub every tool result in `message` whose text is over `cap` bytes:
/// `"[tool result truncated — {n} chars]"` plus a short preview. Returns
/// whether anything changed.
fn cap_tool_results(message: &mut Message, cap: usize) -> bool {
    map_tool_result_text(message, |text| {
        (text.len() > cap).then(|| {
            let preview_chars = (cap / 2).min(200);
            let preview: String = text.chars().take(preview_chars).collect();
            format!("[tool result truncated — {} chars]\n{preview}…", text.len())
        })
    })
}

/// Replace every tool result body in `message` longer than the preview with
/// a one-liner: `"[compacted] {first 120 chars}…"`. Returns whether anything
/// changed.
fn compact_tool_results(message: &mut Message) -> bool {
    map_tool_result_text(message, |text| {
        let trimmed = text.trim();
        (trimmed.chars().count() > COMPACT_PREVIEW_CHARS).then(|| {
            let summary: String = trimmed.chars().take(COMPACT_PREVIEW_CHARS).collect();
            format!("[compacted] {summary}…")
        })
    })
}

/// Apply `rewrite` to the text of every tool-result block in `message` — a
/// `Text` block's text, or a `Json` block's serialized value, which is what
/// the provider sends. A block `rewrite` returns `Some` for becomes a `Text`
/// block with the replacement; images are left alone.
fn map_tool_result_text(message: &mut Message, rewrite: impl Fn(&str) -> Option<String>) -> bool {
    let Message::User { content } = message else {
        return false;
    };
    let mut changed = false;
    for item in content.iter_mut() {
        let UserContent::ToolResult(ToolResult { content, .. }) = item else {
            continue;
        };
        for block in content.iter_mut() {
            let replacement = match block {
                ToolResultContent::Text(text) => rewrite(&text.text),
                ToolResultContent::Json { value } => rewrite(&value.to_string()),
                ToolResultContent::Image(_) => None,
            };
            if let Some(replacement) = replacement {
                *block = ToolResultContent::Text(Text::new(replacement));
                changed = true;
            }
        }
    }
    changed
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::{AssistantContent, ToolCallId};

    fn user_text(s: &str) -> Message {
        Message::User {
            content: vec![UserContent::Text(Text::new(s.to_string()))],
        }
    }

    fn tool_result(id: &str, content: ToolResultContent) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: ToolCallId::new(id).unwrap(),
                provider: None,
                name: "test_tool".to_string(),
                content: vec![content],
            })],
        }
    }

    /// A tool result of `words` words of prose, so its token count is
    /// predictable and its byte length is `words * 5 - 1`.
    fn text_result(id: &str, words: usize) -> Message {
        tool_result(
            id,
            ToolResultContent::Text(Text::new(vec!["word"; words].join(" "))),
        )
    }

    fn json_result(id: &str, words: usize) -> Message {
        tool_result(
            id,
            ToolResultContent::Json {
                value: serde_json::json!({ "content": vec!["word"; words].join(" ") }),
            },
        )
    }

    fn result_text(message: &Message) -> String {
        let Message::User { content } = message else {
            panic!("expected a user message");
        };
        match content.first() {
            Some(UserContent::ToolResult(tr)) => match tr.content.first() {
                Some(ToolResultContent::Text(t)) => t.text.clone(),
                Some(ToolResultContent::Json { value }) => value.to_string(),
                other => panic!("unexpected block {other:?}"),
            },
            Some(UserContent::Text(t)) => t.text.clone(),
            other => panic!("unexpected content {other:?}"),
        }
    }

    fn settings(cap: usize) -> ContextShaperSettings {
        ContextShaperSettings {
            tool_result_cap_bytes: cap,
            keep_head: 1,
            keep_tail: 2,
            response_reserve: 0,
            headroom_fraction: 0.0,
            ..ContextShaperSettings::default()
        }
    }

    fn counter() -> TokenCounter {
        TokenCounter::for_model("test")
    }

    /// A guard whose budget is exactly `tokens(history) - 1`: over by one,
    /// so the cheapest stage that frees anything is the one that fires.
    fn shaper_just_over(history: &[Message], settings: ContextShaperSettings) -> ContextShaper {
        let counter = counter();
        let total: usize = history.iter().map(|m| counter.count_message(m)).sum();
        ContextShaper::new(settings, counter, Some(total - 1))
    }

    #[test]
    fn budget_takes_reserve_base_and_prompt_off_the_window() {
        let shaper = ContextShaper::new(
            ContextShaperSettings {
                response_reserve: 100,
                headroom_fraction: 0.1,
                ..ContextShaperSettings::default()
            },
            counter(),
            Some(1_000),
        );
        shaper.set_base_tokens(300);
        assert_eq!(
            shaper.history_budget(50),
            450,
            "1000 − 100 headroom − 100 reserve − 300 base − 50 prompt"
        );
        assert_eq!(
            shaper.history_budget(10_000),
            0,
            "saturates, never underflows"
        );
    }

    #[test]
    fn a_model_without_a_window_gets_the_assumed_one() {
        let config = ModelConfig::new(
            "id".into(),
            "name".into(),
            crate::settings::models::providers_store::ProviderType::Ollama,
            "qwen3".into(),
        );
        let shaper = ContextShaper::for_model(&config);
        assert_eq!(shaper.context_window(), ASSUMED_CONTEXT_WINDOW);
        assert_eq!(
            shaper.history_budget(0),
            ASSUMED_CONTEXT_WINDOW - ASSUMED_CONTEXT_WINDOW / 10 - RESPONSE_RESERVE
        );
    }

    #[test]
    fn a_configured_window_and_max_tokens_win() {
        let mut config = ModelConfig::new(
            "id".into(),
            "name".into(),
            crate::settings::models::providers_store::ProviderType::OpenRouter,
            "vendor/model".into(),
        );
        config.max_context_window = Some(200_000);
        config.max_tokens = Some(8_000);
        let shaper = ContextShaper::for_model(&config);
        assert_eq!(shaper.history_budget(0), 200_000 - 20_000 - 8_000);
    }

    #[test]
    fn a_history_that_fits_is_left_alone() {
        let history = vec![user_text("hello"), text_result("t1", 50)];
        let shaper = ContextShaper::new(settings(100), counter(), Some(10_000));
        assert!(shaper.shape(&history, 10).is_none());
    }

    /// The AGE-500 shape: the run is over budget, and there is old material
    /// to give up before the tail is touched.
    #[test]
    fn caps_old_results_before_touching_the_tail() {
        let history = vec![
            user_text("task"),
            text_result("old", 200),
            user_text("ok"),
            text_result("recent", 100),
        ];
        let shaper = shaper_just_over(&history, settings(100));
        let shaped = shaper.shape(&history, 0).expect("over budget");
        assert_eq!(shaped.stage_applied, ContextShaperStage::Cap);
        assert!(
            result_text(&shaped.messages[1]).starts_with("[tool result truncated — 999 chars]")
        );
        assert_eq!(
            result_text(&shaped.messages[3]),
            result_text(&history[3]),
            "the tail stays whole"
        );
        assert!(shaped.tokens_after <= shaper.history_budget(0));
        assert!(shaped.tokens_after < shaped.tokens_before);
    }

    #[test]
    fn json_results_count_and_cap_like_text() {
        let history = vec![
            user_text("task"),
            json_result("old", 200),
            user_text("ok"),
            json_result("recent", 100),
        ];
        let shaper = shaper_just_over(&history, settings(100));
        let shaped = shaper
            .shape(&history, 0)
            .expect("a JSON result is not invisible");
        assert_eq!(shaped.stage_applied, ContextShaperStage::Cap);
        assert!(result_text(&shaped.messages[1]).starts_with("[tool result truncated — "));
        assert_eq!(result_text(&shaped.messages[3]), result_text(&history[3]));
    }

    #[test]
    fn one_lines_before_snipping() {
        let mut history = vec![user_text("task")];
        for i in 0..4 {
            history.push(text_result(&format!("t{i}"), 100));
        }
        // Every result is under the 1 KB cap, so capping frees nothing;
        // one-lining the two outside the tail does, and nothing is dropped.
        let shaper = shaper_just_over(&history, settings(1_000));
        let shaped = shaper.shape(&history, 0).expect("over budget");
        assert_eq!(shaped.stage_applied, ContextShaperStage::Compact);
        assert_eq!(shaped.messages.len(), history.len());
        assert!(result_text(&shaped.messages[1]).starts_with("[compacted] "));
        assert!(result_text(&shaped.messages[2]).starts_with("[compacted] "));
        assert_eq!(result_text(&shaped.messages[4]), result_text(&history[4]));
    }

    #[test]
    fn snips_when_capping_and_compacting_free_nothing() {
        // Six results too short to cap or one-line: only dropping helps.
        let mut history = vec![user_text("task")];
        for i in 0..6 {
            history.push(text_result(&format!("t{i}"), 20));
        }
        let shaper = shaper_just_over(&history, settings(1_000));
        let shaped = shaper.shape(&history, 0).expect("over budget");
        assert_eq!(shaped.stage_applied, ContextShaperStage::Snip);
        assert_eq!(shaped.messages.len(), 1 + 1 + 2);
        assert!(result_text(&shaped.messages[1]).contains("4 messages snipped"));
        assert_eq!(result_text(&shaped.messages[3]), result_text(&history[6]));
    }

    /// Every tool result in `messages` is answered by a call before it, and
    /// every call is answered after it — what the providers check.
    fn assert_round_trips_intact(messages: &[Message]) {
        let mut called: HashSet<ToolCallId> = HashSet::new();
        for (index, message) in messages.iter().enumerate() {
            for id in result_ids(message) {
                assert!(
                    called.contains(id),
                    "message {index} answers {id:?}, which nothing before it called"
                );
            }
            called.extend(call_ids(message).cloned());
        }
        let answered: HashSet<&ToolCallId> = messages.iter().flat_map(result_ids).collect();
        for (index, message) in messages.iter().enumerate() {
            for id in call_ids(message) {
                assert!(
                    answered.contains(id),
                    "message {index} calls {id:?}, which nothing answers"
                );
            }
        }
    }

    fn tool_call(id: &str) -> Message {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::tool_call(
                id,
                "test_tool",
                serde_json::json!({}),
            )],
        }
    }

    /// `n` tool round-trips after a user message: the shape every step of a
    /// live tool loop has, and the one the in-loop guard shapes.
    fn tool_loop(n: usize) -> Vec<Message> {
        let mut history = vec![user_text("task")];
        for i in 0..n {
            history.push(tool_call(&format!("t{i}")));
            history.push(text_result(&format!("t{i}"), 20));
        }
        history
    }

    /// The request a live tool loop makes: the history ends on the call, and
    /// the result answering it is the prompt rig passes separately. Nothing
    /// is dangling, so the request goes out exactly as recorded — the
    /// AGE-277 property, and the thing a naive validator would break by
    /// synthesizing a second answer for the id the prompt carries.
    #[test]
    fn a_call_the_prompt_answers_is_not_a_reason_to_patch() {
        let history = vec![user_text("task"), tool_call("t0")];
        let prompt = text_result("t0", 20);
        let shaper = ContextShaper::new(settings(1_000), counter(), Some(10_000));

        assert_eq!(shaper.request_history(&history, &prompt, 2), None);
    }

    /// A history that fits but carries an orphaned tool result is still
    /// patched: the guard is the last place that can catch it, whatever left
    /// it that way (AGE-513).
    #[test]
    fn an_orphaned_result_is_repaired_even_under_budget() {
        let history = vec![user_text("task"), text_result("nobody-called-this", 20)];
        let shaper = ContextShaper::new(settings(1_000), counter(), Some(10_000));

        let patched = shaper
            .request_history(&history, &user_text("and now?"), 1)
            .expect("a malformed history is patched even when it fits");

        assert_eq!(patched.len(), 1, "the orphaned result is gone");
        assert_round_trips_intact(&patched);
    }

    /// Shaping and repair compose: an over-budget history that also ends on
    /// a call nothing answers comes back both shorter and whole.
    #[test]
    fn a_shaped_history_is_repaired_too() {
        let mut history = tool_loop(3);
        // The run was cut off after this call and before its result.
        history.push(tool_call("never-answered"));
        let settings = ContextShaperSettings {
            keep_head: 1,
            keep_tail: 2,
            ..settings(1_000)
        };
        let shaper = shaper_just_over(&history, settings);

        // The prompt is a new user turn, so it answers nothing.
        let patched = shaper
            .request_history(&history, &user_text("and now?"), 4)
            .expect("over budget");

        assert!(patched.len() < history.len(), "still shaped");
        assert_round_trips_intact(&patched);
        assert!(
            result_text(patched.last().expect("a repaired history is not empty"))
                .starts_with("Cancelled:"),
            "the unanswered call got its placeholder"
        );
    }

    /// The AGE-512 400: the cut fell between an assistant `tool_calls`
    /// message and the result answering it, and the provider rejected the
    /// next request with "messages with role 'tool' must be a response to a
    /// preceding message with 'tool_calls'".
    #[test]
    fn snip_keeps_the_call_the_tail_answers() {
        let history = tool_loop(3);
        let settings = ContextShaperSettings {
            keep_head: 1,
            keep_tail: 3,
            ..settings(1_000)
        };
        // Unadjusted, the tail would start on `t1`'s result, whose call is
        // the message before it.
        assert!(matches!(history[history.len() - 3], Message::User { .. }));

        let shaper = shaper_just_over(&history, settings);
        let shaped = shaper.shape(&history, 0).expect("over budget");

        assert_eq!(shaped.stage_applied, ContextShaperStage::Snip);
        assert_round_trips_intact(&shaped.messages);
        assert!(result_text(&shaped.messages[1]).contains("2 messages snipped"));
        assert!(
            call_ids(&shaped.messages[2]).any(|id| id.as_ref() == "t1"),
            "the kept tail opens with the call it answers"
        );
    }

    /// The other end of the same cut: the kept head must not end on a call
    /// whose result the snip drops.
    #[test]
    fn snip_never_keeps_a_head_call_it_drops_the_result_of() {
        let history = tool_loop(3);
        let settings = ContextShaperSettings {
            keep_head: 2,
            keep_tail: 2,
            ..settings(1_000)
        };
        // Unadjusted, the head would end on `t0`'s call, whose result is the
        // first message the snip drops.
        assert!(call_ids(&history[1]).any(|id| id.as_ref() == "t0"));

        let shaper = shaper_just_over(&history, settings);
        let shaped = shaper.shape(&history, 0).expect("over budget");

        assert_eq!(shaped.stage_applied, ContextShaperStage::Snip);
        assert_round_trips_intact(&shaped.messages);
        assert_eq!(shaped.messages.len(), 1 + 1 + 2);
    }

    /// Two fetches and nothing older — trial `2dfc4c37…` in AGE-500. The
    /// older one is stubbed, the newest stays whole.
    #[test]
    fn caps_the_tail_oldest_first_and_never_the_last_message() {
        let history = vec![
            user_text("task"),
            text_result("first-fetch", 300),
            text_result("second-fetch", 200),
        ];
        let shaper = shaper_just_over(&history, settings(100));
        let shaped = shaper.shape(&history, 0).expect("over budget");
        assert_eq!(shaped.stage_applied, ContextShaperStage::CapTail);
        assert!(
            result_text(&shaped.messages[1]).starts_with("[tool result truncated — 1499 chars]")
        );
        assert_eq!(result_text(&shaped.messages[2]), result_text(&history[2]));
    }

    #[test]
    fn a_history_still_over_budget_is_sent_shaped_not_dropped() {
        let history = vec![user_text("task"), text_result("only", 1_000)];
        let shaper = ContextShaper::new(settings(100), counter(), Some(10));
        let shaped = shaper.shape(&history, 0).expect("over budget");
        assert_eq!(shaped.messages.len(), 2);
        assert_eq!(
            result_text(&shaped.messages[1]),
            result_text(&history[1]),
            "the last message is never touched"
        );
        assert!(shaped.tokens_after > shaper.history_budget(0));
    }

    #[test]
    fn the_prompt_counts_against_the_budget() {
        let history = vec![user_text("task"), text_result("old", 100), user_text("ok")];
        let counter = counter();
        let total: usize = history.iter().map(|m| counter.count_message(m)).sum();
        let shaper = ContextShaper::new(settings(100), counter, Some(total));
        assert!(shaper.shape(&history, 0).is_none());
        assert!(
            shaper.shape(&history, 1).is_some(),
            "a large prompt leaves less for the history"
        );
    }

    #[test]
    fn images_are_left_alone() {
        use rig_core::completion::message::{DocumentSourceKind, Image, ImageMediaType};
        let mut message = tool_result(
            "img",
            ToolResultContent::Image(Image {
                data: DocumentSourceKind::base64("AAAA"),
                media_type: Some(ImageMediaType::PNG),
                detail: None,
                additional_params: None,
            }),
        );
        assert!(!cap_tool_results(&mut message, 1));
        assert!(!compact_tool_results(&mut message));
    }

    #[test]
    fn a_result_under_the_recording_cap_is_recorded_whole() {
        let shaper = ContextShaper::new(settings(100), counter(), Some(10_000));
        let output =
            ToolOutput::json(serde_json::json!({ "content": vec!["word"; 200].join(" ") }));
        assert!(shaper.record_tool_output(&output).is_none());
    }

    #[test]
    fn a_result_over_the_recording_cap_is_cut_to_the_cap_once() {
        // Budget 2 000 → cap 800 tokens; 3 000 words are ~3 000 tokens.
        let shaper = ContextShaper::new(settings(100), counter(), Some(2_000));
        let words = vec!["word"; 3_000].join(" ");
        let output = ToolOutput::json(serde_json::json!({ "content": words }));
        let recorded = shaper.record_tool_output(&output).expect("over the cap");
        let text = recorded.as_text().expect("recorded as text");
        assert!(text.starts_with("[tool result truncated — kept "));
        let kept = shaper.counter().count(text);
        assert!(
            kept <= shaper.recording_cap() + 40,
            "kept {kept} tokens against a cap of {}",
            shaper.recording_cap()
        );
        assert!(
            kept > shaper.recording_cap() / 2,
            "keeps most of what fits, not a stub"
        );
        // Recording again is a no-op: the truncated form fits.
        assert!(shaper.record_tool_output(&recorded).is_none());
    }

    #[test]
    fn the_recording_cap_has_a_floor() {
        let shaper = ContextShaper::new(settings(100), counter(), Some(10));
        assert_eq!(shaper.recording_cap(), RECORDING_CAP_FLOOR_TOKENS);
    }

    #[test]
    fn a_result_with_an_image_is_recorded_whole() {
        use rig_core::completion::message::{DocumentSourceKind, Image, ImageMediaType};
        let shaper = ContextShaper::new(settings(100), counter(), Some(10));
        let output = ToolOutput::content(vec![
            ToolResultContent::Text(Text::new(vec!["word"; 3_000].join(" "))),
            ToolResultContent::Image(Image {
                data: DocumentSourceKind::base64("AAAA"),
                media_type: Some(ImageMediaType::PNG),
                detail: None,
                additional_params: None,
            }),
        ])
        .unwrap();
        assert!(shaper.record_tool_output(&output).is_none());
    }
}
