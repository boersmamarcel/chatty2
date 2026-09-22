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
//! A history that is still over budget after stage 4 is sent as is: without
//! summarising there is nothing left to take.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, RequestPatch,
};
use rig_core::completion::Message;
use rig_core::completion::message::{Text, ToolResult, ToolResultContent};
use rig_core::message::UserContent;
use tracing::{debug, warn};

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

        // Stage 3: drop the middle.
        if !fits(&counts) && len > settings.keep_head + settings.keep_tail + 1 {
            stage = ContextShaperStage::Snip;
            let head = settings.keep_head;
            let dropped = tail_start - head;
            let marker = Message::User {
                content: vec![UserContent::Text(Text::new(format!(
                    "[CONTEXT SHAPER: {dropped} messages snipped to reduce context size]"
                )))],
            };
            let marker_tokens = counter.count_message(&marker);
            let tail = messages.split_off(tail_start);
            let tail_counts = counts.split_off(tail_start);
            messages.truncate(head);
            counts.truncate(head);
            messages.push(marker);
            counts.push(marker_tokens);
            messages.extend(tail);
            counts.extend(tail_counts);
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
        let prompt_tokens = self.inner.counter.count_message(event.prompt);
        match self.shape(event.history, prompt_tokens) {
            Some(shaped) => {
                debug!(
                    turn = event.turn,
                    stage = ?shaped.stage_applied,
                    tokens_before = shaped.tokens_before,
                    tokens_after = shaped.tokens_after,
                    budget = self.history_budget(prompt_tokens),
                    "context shaper applied before model call"
                );
                CompletionCallAction::patch(RequestPatch::new().history(shaped.messages))
            }
            None => CompletionCallAction::Continue,
        }
    }
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
    use rig_core::completion::message::ToolCallId;

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
}
