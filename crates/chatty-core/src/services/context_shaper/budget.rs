//! Window and budget arithmetic of the context guard: what the model's window
//! leaves for history, for a reply, and when a request is over it. Pure
//! numbers over the shaper's settings and counter; no transport, no messaging.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rig_core::completion::Message;

use super::ContextShaper;
use crate::token_budget::counter::TokenCounter;

/// The context window assumed for a model that has none configured. Every
/// local model chatty is run against today fits in 32k; a larger window only
/// costs a model some history it could have kept.
pub(super) const ASSUMED_CONTEXT_WINDOW: usize = 32_768;

/// Tokens left for the model's reply when it has no `max_tokens` configured.
pub(super) const RESPONSE_RESERVE: usize = 4_096;

/// Fraction of the window kept free for what the count cannot see: the chat
/// template's wrapping and the tokenizer mismatch (see the module docs).
pub(super) const HEADROOM_FRACTION: f64 = 0.10;

/// Share of the window a run keeps for the reply once a turn of it ran out
/// of window before answering ([`ReplyRoom`], AGE-967).
pub const REPLY_ROOM_SHARE: f64 = 0.5;

/// A clamped `max_tokens` never goes below this: a request with less room
/// than that is over the window anyway, and the provider's refusal says so
/// more clearly than a reply cut off after a few words.
pub const MIN_CLAMPED_REPLY_TOKENS: usize = 1_024;

/// What a request's output cap keeps free beyond the counted input, for the
/// tokenizer gap (AGE-985): our count is cl100k, and a model's own tokenizer
/// counts more on some text, so `input + max_tokens` that adds up to the
/// window by our count can be over it by the model's. The margin is
/// `fraction` of the counted input plus a fixed `slack`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputMargin {
    /// Share of the counted input.
    pub fraction: f64,
    /// Tokens on top, for the chat template and rounding.
    pub slack: usize,
}

impl Default for OutputMargin {
    fn default() -> Self {
        Self {
            fraction: 0.10,
            slack: 256,
        }
    }
}

impl OutputMargin {
    /// The tokens to keep free for a request counted at `input_tokens`.
    pub fn for_input(&self, input_tokens: usize) -> usize {
        (input_tokens as f64 * self.fraction.max(0.0)) as usize + self.slack
    }
}

/// Run-scoped marker (rig scratchpad): a turn of this run ran out of window
/// before it answered, so every later request keeps [`REPLY_ROOM_SHARE`] of
/// the window for the reply. Set by `EmptyTurnRetry` (AGE-967).
#[derive(Clone, Copy, Debug, Default)]
pub struct ReplyRoom;

/// Run-scoped marker (rig scratchpad): the guard lowered a request's
/// `max_tokens` of this run to the room the window had left, so a cut-off
/// at that cap was the window's. Read by `EmptyTurnRetry` (AGE-967).
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowClamped;

/// Extra headroom, as a share of the window, per request the provider
/// refused as over its window ([`WindowOverflows`], AGE-978).
pub(super) const OVERFLOW_HEADROOM_STEP: f64 = 0.10;

/// Refusals counted towards the extra headroom: three tenths of the window
/// more is already far past any tokenizer gap measured.
pub(super) const MAX_OVERFLOW_STEPS: usize = 3;

/// How many of an agent's requests its provider refused as over the context
/// window (AGE-978). Shared by the agent's context guard, which keeps more
/// headroom per refusal, and the HTTP layer that sees the refusal.
#[derive(Clone, Debug, Default)]
pub struct WindowOverflows(Arc<AtomicUsize>);

impl WindowOverflows {
    /// Count one refused request.
    pub fn record(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// Refusals counted so far.
    pub fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

impl ContextShaper {
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

    /// The refusal count this guard keeps more headroom for; the HTTP layer
    /// under the agent's model records on it (AGE-978).
    pub fn window_overflows(&self) -> WindowOverflows {
        self.inner.overflows.clone()
    }

    /// The share of `window` kept free for what the count cannot see: the
    /// configured fraction, plus a step per refused request.
    pub(super) fn headroom(&self, window: usize) -> usize {
        let steps = self.inner.overflows.count().min(MAX_OVERFLOW_STEPS);
        let fraction =
            self.inner.settings.headroom_fraction + OVERFLOW_HEADROOM_STEP * steps as f64;
        (window as f64 * fraction) as usize
    }

    /// Tokens available to the history once the headroom, the reply reserve,
    /// the base and `prompt_tokens` are taken off the window.
    pub fn history_budget(&self, prompt_tokens: usize) -> usize {
        let window = self.context_window();
        let headroom = self.headroom(window);
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

    /// The tokens a run that ran out of window keeps free for the reply on
    /// top of the usual reserve ([`ReplyRoom`]): enough for the reply to
    /// have [`REPLY_ROOM_SHARE`] of the window.
    pub fn reply_room_reserve(&self) -> usize {
        let room = (self.context_window() as f64 * REPLY_ROOM_SHARE) as usize;
        room.saturating_sub(self.inner.settings.response_reserve)
    }

    /// The `max_tokens` a request carrying `history` and `prompt` should go
    /// out with instead of the agent's cap, or `None` when the cap fits (or
    /// there is none): the window less the headroom, the base and the
    /// request, never under [`MIN_CLAMPED_REPLY_TOKENS`]. Only against a
    /// window the model is configured with: the assumed 32k may be far
    /// below a hosted model's real one, and a cap clamped to it would cut
    /// replies short that always fit.
    pub fn clamped_output_cap(&self, history: &[Message], prompt: &Message) -> Option<u64> {
        let cap = self.inner.settings.output_cap?;
        let room = self.reply_room_left(history, prompt)?;
        (cap > room).then(|| room.max(MIN_CLAMPED_REPLY_TOKENS) as u64)
    }

    /// The tokens a request carrying `history` and `prompt` leaves for the
    /// reply in the model's configured window, less the headroom and the
    /// base; `None` without a configured window.
    pub(super) fn reply_room_left(&self, history: &[Message], prompt: &Message) -> Option<usize> {
        let window = self.inner.context_window?;
        let request = self.request_tokens(history, prompt);
        let base = self.inner.base_tokens.load(Ordering::Relaxed);
        // The window headroom, or the margin on the counted input when that
        // is more (AGE-985): the input is undercounted by a share of itself.
        let margin = self
            .headroom(window)
            .max(self.inner.settings.output_margin.for_input(base + request));
        Some(
            window
                .saturating_sub(margin)
                .saturating_sub(base)
                .saturating_sub(request),
        )
    }

    /// `history` and `prompt` in tokens: a request's messages.
    pub(super) fn request_tokens(&self, history: &[Message], prompt: &Message) -> usize {
        let counter = &self.inner.counter;
        history
            .iter()
            .chain(std::iter::once(prompt))
            .map(|m| counter.count_message(m))
            .sum()
    }

    /// Why a request carrying `history` (as shaped) and `prompt` cannot be
    /// sent: the history is what takes it over the model's configured
    /// window, so the provider would refuse it whatever the reply (AGE-994).
    /// `None` when it can go; when the model has no configured window (the
    /// assumed one may be far below a hosted model's real one); and when
    /// the base and the prompt alone fill the window, which no shaping of
    /// the history changes and the cut-off ladder reports (AGE-967).
    pub fn window_overflow(&self, history: &[Message], prompt: &Message) -> Option<String> {
        let window = self.inner.context_window?;
        let base = self.inner.base_tokens.load(Ordering::Relaxed);
        let fixed = base + self.inner.counter.count_message(prompt);
        let tokens = base + self.request_tokens(history, prompt);
        (fixed <= window && tokens > window).then(|| {
            format!(
                "the conversation does not fit the model's context window of {window} tokens: \
                 shaped as far as it goes — the first message, the latest user message, the \
                 last message and the new prompt kept — the request still counts {tokens} \
                 tokens, {base} of them the system prompt and tool schemas. Start a new \
                 conversation or send a shorter message."
            )
        })
    }

    /// Whether this agent's latest request left less room for the reply
    /// than [`MIN_CLAMPED_REPLY_TOKENS`] in its configured window: the
    /// clamp could not make it fit, and a cut-off there is the window being
    /// full, not `max_tokens` being too low (AGE-967).
    pub fn window_exhausted(&self) -> bool {
        self.inner.window_exhausted.load(Ordering::Relaxed)
    }
}
