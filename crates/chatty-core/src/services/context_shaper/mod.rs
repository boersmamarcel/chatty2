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
//! # Compaction
//!
//! In unattended runs, before any of the stages below, a history that has grown past
//! [`COMPACTION_TRIGGER_FRACTION`] of the budget is *compacted*: its older
//! part is replaced by one summary message ([`super::context_compaction`])
//! carrying the task verbatim, the files written so far, a `git status`
//! snapshot, the last command's output and a short progress note from one
//! tool-free model call, and the most recent messages stay verbatim.
//! Compaction is a rare, discrete event: the guard remembers it, and every
//! later request of the agent sends the same summary followed by the history
//! after it, so the request prefix stays stable (prompt caching) until the
//! next compaction. Like the stages, it is never persisted: the conversation
//! keeps every message.
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
//! | 0 | Terminal context | Stub every `<terminal_context>` snapshot but the newest (AGE-587), and the `<file_context>` blocks of every message but the newest that has any (AGE-2169) |
//! | 1 | Cap | Stub every tool result over `tool_result_cap_bytes` outside the tail |
//! | 2 | Compact | Replace every tool result outside the tail with a one-liner |
//! | 3 | Snip | Keep the first `keep_head` and last `keep_tail` messages and the latest user message, drop the rest |
//! | 4 | Cap the tail | Stub, then one-line, tool results inside the tail, oldest first, never the last message |
//! | 5 | Trim | Drop the kept tail's messages too, oldest first and a whole round-trip at a time, never the latest user message or the last message |
//!
//! Stage 3's cut steps off a tool round-trip before it fires: the kept tail
//! starts no later than the assistant message issuing the calls it answers,
//! and the kept head ends no later than the last call the snip still answers.
//! Dropping half a round-trip is a 400 from every OpenAI-compatible provider,
//! and since the guard runs inside a live tool loop that is the cut's usual
//! shape, not an edge case (AGE-512).
//!
//! Stage 5 is what fits a new turn of a long conversation (AGE-994): at
//! turn start the history is every earlier turn, and its bulk is assistant
//! text — replies, reasoning, the code a tool call carries — which no stage
//! before it shrinks. The latest user message is kept by stages 3 and 5
//! wherever it is: mid-run it is what the run is answering, and the head
//! keeps the task's first message.
//!
//! A history still over budget after stage 5 — the head, the latest user
//! message and the last message alone do not fit — is sent as is, unless
//! the request then counts more than the model's configured window: that
//! is a certain refusal, and the hook stops the run with an error saying
//! so ([`ContextShaper::window_overflow`]) instead of sending it.
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
//!
//! A cut result keeps its head and its tail, with a marker in the middle
//! saying how much was left out and how to read it (AGE-978): a data file's
//! last rows, a log's final error, are often what the model needs. And the
//! results of one tool round all go out in that one unshaped message, so
//! each is capped at two fifths of the room the round's earlier results
//! left, not of the whole budget: four parallel reads no longer add up to
//! more than the window ([`ContextShaper::record_round_output`]).
//!
//! # A refused request (AGE-978)
//!
//! The count is an estimate, and on some text (digits, for one) a model's
//! own tokenizer counts far more than it. When a provider still refuses a
//! request as over its window, the HTTP layer under rig compacts that one
//! request and sends it again (`connect_retry_http`), and records the
//! refusal on [`WindowOverflows`]: every later request of the agent keeps
//! another tenth of the window as headroom, up to three tenths, so the
//! guard shapes it before the provider has to refuse it.
//!
//! # The results ledger (AGE-941)
//!
//! For a leader, the old tool results the stages one-line and snip are its
//! workers' answers. Whenever a request goes out shaped or compacted and no
//! longer carries some `invoke_agent` result verbatim, the guard adds one
//! message right after the head: a capped ledger of those delegations
//! ([`super::results_ledger`]) — agent, task, status and a digest of the
//! answer — telling the model to take figures only from it or from results
//! still shown, and to write "not recorded" otherwise. Room for it is made
//! by shaping again with its size counted against the budget. A request
//! that goes out as recorded, or that drops no delegation result, is
//! unchanged.
//!
//! # Room for the reply (AGE-967)
//!
//! Two things keep a request's reply inside the window. When the agent has
//! an output cap (`max_tokens`), the model has a configured window, and the
//! request as sent leaves less room than that cap — the history could not
//! be shaped small enough, or the prompt alone is large — the request's
//! `max_tokens` is lowered to the room left (never under
//! [`MIN_CLAMPED_REPLY_TOKENS`]), since vLLM refuses a request whose prompt
//! plus `max_tokens` exceeds its window; the run's scratchpad then carries
//! [`WindowClamped`]. And once a turn was cut off with no answer because
//! the window ran out (the cut-off ladder in `empty_turn_retry`), the run's
//! scratchpad carries [`ReplyRoom`], and every later request of the run
//! keeps [`REPLY_ROOM_SHARE`] of the window for the reply: the history is
//! shaped, or compacted, that much harder. That window is the model's
//! `max_context_window`, else the assumed 32k; the clamp needs a configured
//! one.
//!
//! # Mid-run messages (TM-5)
//!
//! A worker's guard is also where the human, or the worker's owner, reaches
//! it mid-run. Once [`ContextShaper::deliver_mid_run_messages`] gives it the
//! worker's broker connection, every tool result it records first takes the
//! worker's mid-run list (`mailbox.take`), and whatever was waiting rides at
//! the end of that result, each message in its `<message … untrusted="true">`
//! wrapper with its body escaped. So it is in the next model request — the
//! tool round's user-role tool-results message — and never in one already on
//! the wire. rig's hooks cannot add a message after a round's results (the
//! post-tool call's prompt *is* that message), so the result carries it; it
//! is recorded once, like the result, and resent unchanged after. It grants
//! nothing: it is data in a tool result. And it costs one prompt-cache break,
//! like any new tool result whose bytes nobody could predict.

mod budget;
mod compaction;
#[cfg(test)]
mod compaction_tests;
mod hook;
mod recording;
#[cfg(test)]
mod results_ledger_tests;
mod stages;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod turn_start_tests;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, OnceLock};

use chatty_fabric::Transport;
use rig_core::completion::Message;

use crate::models::token_usage::ApiCallUsage;
use crate::services::context_compaction::Summarizer;
use crate::settings::models::models_store::ModelConfig;
use crate::token_budget::counter::TokenCounter;

use self::budget::{ASSUMED_CONTEXT_WINDOW, HEADROOM_FRACTION, RESPONSE_RESERVE};
// Names the test modules reach through `use super::*`.
#[cfg(test)]
use {
    self::stages::{cap_tool_results, compact_tool_results, latest_user_message},
    crate::services::{call_ids, result_ids, tool_round_trips_intact},
    rig_core::completion::message::{CallId, Text, ToolResult, ToolResultContent},
    rig_core::message::UserContent,
    rig_core::tool::ToolOutput,
    std::collections::HashSet,
};

pub use self::budget::{
    MIN_CLAMPED_REPLY_TOKENS, OutputMargin, REPLY_ROOM_SHARE, ReplyRoom, WindowClamped,
    WindowOverflows,
};
use self::compaction::Compaction;
pub use self::compaction::{COMPACTION_KEEP_RECENT_FRACTION, COMPACTION_TRIGGER_FRACTION};
pub use self::recording::RECORDING_CAP_FLOOR_TOKENS;
pub(crate) use self::recording::{head_and_tail, omitted_marker};
use self::stages::{KEEP_HEAD, KEEP_TAIL, TOOL_RESULT_CAP_BYTES};

// ── Public API ────────────────────────────────────────────────────────────────

/// Which stage was the last one applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextShaperStage {
    /// Older terminal snapshots and dragged files stubbed (AGE-587, AGE-2169).
    TerminalContext,
    Cap,
    Compact,
    Snip,
    CapTail,
    /// Messages of the kept tail dropped, oldest round-trip first (AGE-994).
    Trim,
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

    /// Share of the budget at which the history is compacted, once
    /// [`ContextShaper::enable_compaction`] has turned compaction on.
    pub compaction_trigger_fraction: f64,

    /// Share of the budget the newest messages keep verbatim on compaction.
    pub compaction_keep_recent_fraction: f64,

    /// The agent's configured output cap (`max_tokens`), which a request is
    /// clamped below when the window has less room left (AGE-967). `None`:
    /// requests carry no cap and are never given one.
    pub output_cap: Option<usize>,

    /// What the output cap keeps free beyond the counted input (AGE-985); a
    /// clamped cap leaves the larger of this and the window headroom.
    pub output_margin: OutputMargin,
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
            compaction_trigger_fraction: COMPACTION_TRIGGER_FRACTION,
            compaction_keep_recent_fraction: COMPACTION_KEEP_RECENT_FRACTION,
            output_cap: None,
            output_margin: OutputMargin::default(),
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
    /// The tool-free model call that writes a compaction's prose. Without
    /// one, compaction falls back to the deterministic summary.
    summarizer: OnceLock<Arc<dyn Summarizer>>,
    /// Where `git status` is taken for a compaction summary.
    workspace: OnceLock<PathBuf>,
    /// The last compaction, reused by every request after it.
    compaction: Mutex<Option<Compaction>>,
    /// Set by [`ContextShaper::enable_compaction`].
    compaction_enabled: AtomicBool,
    /// Usage of the summary calls made since the stream last took it
    /// ([`ContextShaper::take_compaction_usage`], AGE-683).
    compaction_usage: Mutex<Vec<ApiCallUsage>>,
    /// A worker's broker connection, its mid-run messages' source (TM-5).
    mid_run: OnceLock<Arc<dyn Transport>>,
    /// Set per request: the request left the reply less than
    /// [`MIN_CLAMPED_REPLY_TOKENS`] of a configured window.
    window_exhausted: AtomicBool,
    /// Tokens recorded by the tool results of the current round (AGE-978).
    round_recorded: Mutex<usize>,
    /// Requests the provider refused as over the window (AGE-978).
    overflows: WindowOverflows,
}

impl ContextShaper {
    /// A guard for `model_config`: its `max_context_window` when it has one,
    /// its `max_tokens` as the reply reserve when set, its tokenizer.
    pub fn for_model(model_config: &ModelConfig) -> Self {
        let mut settings = ContextShaperSettings::default();
        if let Some(max_tokens) = model_config.max_tokens.filter(|n| *n > 0) {
            settings.response_reserve = max_tokens as usize;
            settings.output_cap = Some(max_tokens as usize);
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
                summarizer: OnceLock::new(),
                workspace: OnceLock::new(),
                compaction: Mutex::new(None),
                compaction_enabled: AtomicBool::new(false),
                compaction_usage: Mutex::new(Vec::new()),
                mid_run: OnceLock::new(),
                window_exhausted: AtomicBool::new(false),
                round_recorded: Mutex::new(0),
                overflows: WindowOverflows::default(),
            }),
        }
    }
}
