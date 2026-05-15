//! Graduated context-shaping pipeline.
//!
//! Before every LLM call, this pipeline can optionally reshape the conversation
//! history to reduce its token footprint. The five stages are applied lazily in
//! order — the pipeline stops as soon as the history fits within the pressure
//! threshold, so "compress as little as you can get away with."
//!
//! # Stages (cheapest → most expensive)
//!
//! | # | Name | Cost | What it does |
//! |---|------|------|--------------|
//! | 1 | **Budget reduction** | free | Trim individual tool-result messages > 8 KB to a short stub |
//! | 2 | **Snip** | free | Drop oldest middle messages when total chars > snip threshold |
//! | 3 | **Micro-compact** | free | Replace middle tool-result blocks with one-line summaries |
//! | 4 | **Context collapse** | 1 LLM call | Summarise middle band via `summarize_oldest_half` |
//! | 5 | **Auto-compact** | 1 LLM call | Full `summarize_oldest_half` pass on the whole history |
//!
//! Stages 4–5 require an [`AgentClient`] and are only reachable when one is
//! provided.  If `agent` is `None`, the pipeline caps at stage 3.
//!
//! ## Usage
//!
//! ```ignore
//! let settings = ContextShaperSettings::default();
//! let shaped = shape_context(history, &settings, None).await;
//! // shaped.messages is the (possibly shortened) history to pass to the LLM.
//! eprintln!("context shaper applied: {:?}", shaped.stage_applied);
//! ```

use rig::OneOrMany;
use rig::completion::Message;
use rig::completion::message::{AssistantContent, Text, ToolResult, ToolResultContent};
use rig::message::UserContent;
use tracing::debug;

use crate::factories::AgentClient;
use crate::token_budget::summarize_oldest_half;

// ── Tunables ──────────────────────────────────────────────────────────────────

/// Tool result payloads larger than this are truncated by stage 1.
const BUDGET_REDUCTION_TOOL_RESULT_BYTES: usize = 8_192;

/// Total history char count above which stage 2 (snip) fires.
const SNIP_THRESHOLD_CHARS: usize = 80_000;

/// Number of messages to keep at the head and tail of history after snipping.
const SNIP_KEEP_HEAD: usize = 2;
const SNIP_KEEP_TAIL: usize = 8;

/// Total char count above which stage 3 (micro-compact) fires after snipping.
const MICRO_COMPACT_THRESHOLD_CHARS: usize = 50_000;

/// Total char count above which stage 4 (context collapse) fires.
const COLLAPSE_THRESHOLD_CHARS: usize = 30_000;

/// Total char count above which stage 5 (auto-compact) fires.
const AUTO_COMPACT_THRESHOLD_CHARS: usize = 20_000;

// ── Public API ────────────────────────────────────────────────────────────────

/// Which stage of the context shaper was applied (or `None` if no change was needed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextShaperStage {
    BudgetReduction,
    Snip,
    MicroCompact,
    Collapse,
    AutoCompact,
}

/// Settings that control the shaping pipeline.
///
/// All fields have sensible defaults. You can override individual thresholds
/// by constructing the struct directly.
#[derive(Debug, Clone)]
pub struct ContextShaperSettings {
    /// Maximum bytes per tool result before stage 1 truncates it.
    pub budget_reduction_tool_result_bytes: usize,

    /// Total-chars threshold that triggers stage 2 (snip).
    pub snip_threshold_chars: usize,

    /// Total-chars threshold that triggers stage 3 (micro-compact).
    pub micro_compact_threshold_chars: usize,

    /// Total-chars threshold that triggers stage 4 (context collapse).
    pub collapse_threshold_chars: usize,

    /// Total-chars threshold that triggers stage 5 (auto-compact).
    pub auto_compact_threshold_chars: usize,

    /// Number of messages to keep at the head of history when snipping.
    pub snip_keep_head: usize,

    /// Number of messages to keep at the tail of history when snipping.
    pub snip_keep_tail: usize,
}

impl Default for ContextShaperSettings {
    fn default() -> Self {
        Self {
            budget_reduction_tool_result_bytes: BUDGET_REDUCTION_TOOL_RESULT_BYTES,
            snip_threshold_chars: SNIP_THRESHOLD_CHARS,
            micro_compact_threshold_chars: MICRO_COMPACT_THRESHOLD_CHARS,
            collapse_threshold_chars: COLLAPSE_THRESHOLD_CHARS,
            auto_compact_threshold_chars: AUTO_COMPACT_THRESHOLD_CHARS,
            snip_keep_head: SNIP_KEEP_HEAD,
            snip_keep_tail: SNIP_KEEP_TAIL,
        }
    }
}

/// Result of a context-shaping pass.
#[derive(Debug, Clone)]
pub struct ShapedContext {
    /// The (possibly compressed) message history to pass to the LLM.
    pub messages: Vec<Message>,

    /// Which stage was the last one applied, if any.  `None` means the history
    /// fit within the first threshold and no transformation was performed.
    pub stage_applied: Option<ContextShaperStage>,

    /// Approximate number of characters freed by the shaping operation.
    pub chars_freed: usize,
}

/// Apply the context-shaping pipeline to `history`.
///
/// Stages are applied in order.  The pipeline stops as soon as `history` fits
/// within the next stage's threshold.  Expensive stages (4–5) are only run
/// when `agent` is provided.
///
/// This function is `async` because stages 4 and 5 make LLM calls.
pub async fn shape_context(
    history: Vec<Message>,
    settings: &ContextShaperSettings,
    agent: Option<&AgentClient>,
) -> ShapedContext {
    let original_chars = total_chars(&history);

    // Stage 1: Budget reduction — trim oversized individual tool results.
    let (history, s1_freed) = stage1_budget_reduction(history, settings);
    let after_s1 = total_chars(&history);
    if s1_freed > 0 {
        debug!(freed = s1_freed, after = after_s1, "context shaper stage1: budget reduction");
    }
    if after_s1 <= settings.snip_threshold_chars {
        return shaped(history, if s1_freed > 0 { Some(ContextShaperStage::BudgetReduction) } else { None }, original_chars);
    }

    // Stage 2: Snip — drop oldest middle messages.
    let (history, s2_freed) = stage2_snip(history, settings);
    let after_s2 = total_chars(&history);
    if s2_freed > 0 {
        debug!(freed = s2_freed, after = after_s2, "context shaper stage2: snip");
    }
    if after_s2 <= settings.micro_compact_threshold_chars {
        let stage = if s2_freed > 0 { ContextShaperStage::Snip } else { ContextShaperStage::BudgetReduction };
        return shaped(history, Some(stage), original_chars);
    }

    // Stage 3: Micro-compact — replace middle tool-result bodies with one-liners.
    let (history, s3_freed) = stage3_micro_compact(history, settings);
    let after_s3 = total_chars(&history);
    if s3_freed > 0 {
        debug!(freed = s3_freed, after = after_s3, "context shaper stage3: micro-compact");
    }
    if after_s3 <= settings.collapse_threshold_chars || agent.is_none() {
        let stage = pick_stage(s1_freed, s2_freed, s3_freed);
        return shaped(history, stage, original_chars);
    }

    let agent = agent.unwrap();

    // Stage 4: Context collapse — summarise middle half via LLM.
    let (history, s4_freed) = stage4_collapse(history, settings, agent).await;
    let after_s4 = total_chars(&history);
    if s4_freed > 0 {
        debug!(freed = s4_freed, after = after_s4, "context shaper stage4: collapse");
    }
    if after_s4 <= settings.auto_compact_threshold_chars {
        let stage = if s4_freed > 0 { Some(ContextShaperStage::Collapse) } else { pick_stage(s1_freed, s2_freed, s3_freed) };
        return shaped(history, stage, original_chars);
    }

    // Stage 5: Auto-compact — full summarize pass.
    let (history, s5_freed) = stage5_auto_compact(history, agent).await;
    let after_s5 = total_chars(&history);
    if s5_freed > 0 {
        debug!(freed = s5_freed, after = after_s5, "context shaper stage5: auto-compact");
    }
    let stage = if s5_freed > 0 { Some(ContextShaperStage::AutoCompact) } else { pick_stage(s1_freed, s2_freed, s3_freed) };
    shaped(history, stage, original_chars)
}

// ── Stage implementations ─────────────────────────────────────────────────────

/// Stage 1: Trim any individual tool-result payload exceeding the byte limit.
///
/// Replaces the oversized text with a stub: `"[tool result truncated — {n} chars]"`.
/// Non-text content (images) is left untouched.
fn stage1_budget_reduction(
    history: Vec<Message>,
    settings: &ContextShaperSettings,
) -> (Vec<Message>, usize) {
    let limit = settings.budget_reduction_tool_result_bytes;
    let mut freed = 0usize;

    let history = history
        .into_iter()
        .map(|msg| match msg {
            Message::User { content } => {
                let content = content
                    .into_iter()
                    .map(|item| match item {
                        UserContent::ToolResult(mut tr) => {
                            tr.content = trim_tool_result_content(tr.content, limit, &mut freed);
                            UserContent::ToolResult(tr)
                        }
                        other => other,
                    })
                    .collect::<Vec<_>>();
                Message::User {
                    content: to_one_or_many(content),
                }
            }
            other => other,
        })
        .collect();

    (history, freed)
}

fn trim_tool_result_content(
    content: OneOrMany<ToolResultContent>,
    limit: usize,
    freed: &mut usize,
) -> OneOrMany<ToolResultContent> {
    let items: Vec<ToolResultContent> = content
        .into_iter()
        .map(|item| match item {
            ToolResultContent::Text(t) if t.text.len() > limit => {
                let original_len = t.text.len();
                // Preview is at most half the limit or 200 chars, whichever is smaller.
                let preview_chars = (limit / 2).min(200);
                let preview: String = t.text.chars().take(preview_chars).collect();
                let stub_text = format!(
                    "[tool result truncated — {original_len} chars]\n{preview}…"
                );
                // freed = chars removed (original minus the stub we wrote).
                *freed += original_len.saturating_sub(stub_text.len());
                ToolResultContent::Text(Text { text: stub_text })
            }
            other => other,
        })
        .collect();
    to_one_or_many(items)
}

/// Stage 2: Snip — drop the oldest middle messages when total is too large.
///
/// Keeps `snip_keep_head` messages at the start and `snip_keep_tail` at the
/// end, replacing the dropped block with a single marker message.
fn stage2_snip(
    history: Vec<Message>,
    settings: &ContextShaperSettings,
) -> (Vec<Message>, usize) {
    let head = settings.snip_keep_head;
    let tail = settings.snip_keep_tail;

    if history.len() <= head + tail + 1 {
        return (history, 0);
    }

    let tail_start = history.len().saturating_sub(tail);
    let drop_start = head;
    let drop_end = tail_start;

    if drop_start >= drop_end {
        return (history, 0);
    }

    let dropped_chars: usize = history[drop_start..drop_end]
        .iter()
        .map(message_chars)
        .sum();

    let marker = Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: format!(
                "[CONTEXT SHAPER: {} messages snipped to reduce context size]",
                drop_end - drop_start
            ),
        })),
    };

    let mut new_history = Vec::with_capacity(head + 1 + tail);
    new_history.extend_from_slice(&history[..head]);
    new_history.push(marker);
    new_history.extend_from_slice(&history[tail_start..]);

    (new_history, dropped_chars)
}

/// Stage 3: Micro-compact — replace middle tool-result text bodies with a
/// one-line summary.  The first `head` and last `tail` messages are left
/// intact so recent context is preserved.
fn stage3_micro_compact(
    history: Vec<Message>,
    settings: &ContextShaperSettings,
) -> (Vec<Message>, usize) {
    let head = settings.snip_keep_head;
    let tail = settings.snip_keep_tail;
    let len = history.len();
    let mut freed = 0usize;

    if len <= head + tail {
        return (history, 0);
    }

    let tail_start = len.saturating_sub(tail);

    let history = history
        .into_iter()
        .enumerate()
        .map(|(i, msg)| {
            // Only compact the middle band.
            if i < head || i >= tail_start {
                return msg;
            }
            match msg {
                Message::User { content } => {
                    let content = content
                        .into_iter()
                        .map(|item| match item {
                            UserContent::ToolResult(tr) => {
                                let (compacted, delta) = micro_compact_tool_result(tr);
                                freed += delta;
                                UserContent::ToolResult(compacted)
                            }
                            other => other,
                        })
                        .collect::<Vec<_>>();
                    Message::User {
                        content: to_one_or_many(content),
                    }
                }
                other => other,
            }
        })
        .collect();

    (history, freed)
}

fn micro_compact_tool_result(mut tr: ToolResult) -> (ToolResult, usize) {
    let mut freed = 0usize;

    tr.content = {
        let items: Vec<ToolResultContent> = tr
            .content
            .into_iter()
            .map(|item| match item {
                ToolResultContent::Text(t) if t.text.len() > 200 => {
                    let original_len = t.text.len();
                    // One-line summary: first 120 chars of trimmed text.
                    let summary: String = t.text.trim().chars().take(120).collect();
                    freed += original_len.saturating_sub(summary.len() + 30);
                    ToolResultContent::Text(Text {
                        text: format!("[compacted] {summary}…"),
                    })
                }
                other => other,
            })
            .collect();
        to_one_or_many(items)
    };

    (tr, freed)
}

/// Stage 4: Context collapse — LLM-summarise the middle portion of history.
async fn stage4_collapse(
    history: Vec<Message>,
    settings: &ContextShaperSettings,
    agent: &AgentClient,
) -> (Vec<Message>, usize) {
    let head = settings.snip_keep_head;
    let tail = settings.snip_keep_tail;
    let len = history.len();

    if len <= head + tail + 2 {
        return (history, 0);
    }

    let tail_start = len.saturating_sub(tail);
    // Summarise the middle band only (not the most recent tail).
    let middle = history[head..tail_start].to_vec();
    let middle_chars: usize = middle.iter().map(message_chars).sum();

    match summarize_oldest_half(agent, &middle).await {
        Ok(result) => {
            let saved = middle_chars.saturating_sub(
                result.new_history.iter().map(message_chars).sum::<usize>(),
            );
            let mut new_history = Vec::with_capacity(head + result.new_history.len() + tail);
            new_history.extend_from_slice(&history[..head]);
            new_history.extend(result.new_history);
            new_history.extend_from_slice(&history[tail_start..]);
            (new_history, saved)
        }
        Err(e) => {
            debug!(error = %e, "context shaper stage4: collapse failed, skipping");
            (history, 0)
        }
    }
}

/// Stage 5: Auto-compact — full `summarize_oldest_half` on the entire history.
async fn stage5_auto_compact(
    history: Vec<Message>,
    agent: &AgentClient,
) -> (Vec<Message>, usize) {
    let original_chars: usize = history.iter().map(message_chars).sum();

    match summarize_oldest_half(agent, &history).await {
        Ok(result) => {
            let new_chars: usize = result.new_history.iter().map(message_chars).sum();
            let saved = original_chars.saturating_sub(new_chars);
            (result.new_history, saved)
        }
        Err(e) => {
            debug!(error = %e, "context shaper stage5: auto-compact failed, skipping");
            (history, 0)
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn total_chars(history: &[Message]) -> usize {
    history.iter().map(message_chars).sum()
}

fn message_chars(msg: &Message) -> usize {
    match msg {
        Message::User { content } => content
            .iter()
            .map(|item| match item {
                UserContent::Text(t) => t.text.len(),
                UserContent::ToolResult(tr) => tr
                    .content
                    .iter()
                    .map(|c| match c {
                        ToolResultContent::Text(t) => t.text.len(),
                        ToolResultContent::Image(_) => 256, // arbitrary placeholder
                    })
                    .sum::<usize>(),
                UserContent::Image(_) => 256,
                UserContent::Audio(_) => 256,
                UserContent::Video(_) => 256,
                UserContent::Document(_) => 256,
            })
            .sum(),
        Message::Assistant { content, .. } => content
            .iter()
            .map(|item| match item {
                AssistantContent::Text(t) => t.text.len(),
                AssistantContent::ToolCall(tc) => tc.function.arguments.to_string().len() + tc.function.name.len(),
                _ => 0,
            })
            .sum(),
        Message::System { content } => content.len(),
    }
}

/// Helper: convert a `Vec<T>` into a `OneOrMany<T>`.
///
/// Falls back to an empty text placeholder if the vec is empty so we never
/// produce an invalid message.
fn to_one_or_many<T>(items: Vec<T>) -> OneOrMany<T>
where
    T: Clone + std::fmt::Debug,
{
    match items.len() {
        0 => unreachable!("to_one_or_many called with empty vec"),
        1 => OneOrMany::one(items.into_iter().next().unwrap()),
        _ => OneOrMany::many(items).expect("non-empty vec always produces OneOrMany"),
    }
}

fn pick_stage(s1: usize, s2: usize, s3: usize) -> Option<ContextShaperStage> {
    if s3 > 0 {
        Some(ContextShaperStage::MicroCompact)
    } else if s2 > 0 {
        Some(ContextShaperStage::Snip)
    } else if s1 > 0 {
        Some(ContextShaperStage::BudgetReduction)
    } else {
        None
    }
}

fn shaped(messages: Vec<Message>, stage_applied: Option<ContextShaperStage>, original_chars: usize) -> ShapedContext {
    let new_chars = total_chars(&messages);
    ShapedContext {
        chars_freed: original_chars.saturating_sub(new_chars),
        messages,
        stage_applied,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn user_text(s: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: s.to_string(),
            })),
        }
    }

    fn tool_result_msg(id: &str, content: &str) -> Message {
        Message::User {
            content: OneOrMany::one(UserContent::ToolResult(ToolResult {
                id: id.to_string(),
                call_id: None,
                content: OneOrMany::one(ToolResultContent::Text(Text {
                    text: content.to_string(),
                })),
            })),
        }
    }

    fn big_tool_result(id: &str, size: usize) -> Message {
        tool_result_msg(id, &"x".repeat(size))
    }

    fn settings_low_thresholds() -> ContextShaperSettings {
        ContextShaperSettings {
            budget_reduction_tool_result_bytes: 100,
            snip_threshold_chars: 500,
            micro_compact_threshold_chars: 300,
            collapse_threshold_chars: 200,
            auto_compact_threshold_chars: 100,
            snip_keep_head: 1,
            snip_keep_tail: 2,
        }
    }

    #[test]
    fn stage1_trims_oversized_tool_result() {
        let msg = big_tool_result("id1", 200);
        let settings = settings_low_thresholds();
        let (out, freed) = stage1_budget_reduction(vec![msg], &settings);
        assert!(freed > 0);
        // Content should be a stub now.
        if let Message::User { content } = &out[0] {
            if let UserContent::ToolResult(tr) = content.first() {
                if let ToolResultContent::Text(t) = tr.content.first() {
                    assert!(t.text.contains("truncated"));
                }
            }
        }
    }

    #[test]
    fn stage1_leaves_small_tool_result_intact() {
        let msg = tool_result_msg("id1", "small content");
        let settings = settings_low_thresholds();
        let (_, freed) = stage1_budget_reduction(vec![msg], &settings);
        assert_eq!(freed, 0);
    }

    #[test]
    fn stage2_snips_middle_messages() {
        let history: Vec<Message> = (0..10).map(|i| user_text(&format!("msg {i}"))).collect();
        let settings = ContextShaperSettings {
            snip_keep_head: 1,
            snip_keep_tail: 2,
            ..settings_low_thresholds()
        };
        let (out, freed) = stage2_snip(history, &settings);
        assert!(freed > 0);
        // head (1) + marker (1) + tail (2) = 4
        assert_eq!(out.len(), 4);
        // The marker should mention "snipped".
        if let Message::User { content } = &out[1] {
            if let UserContent::Text(t) = content.first() {
                assert!(t.text.contains("snipped"));
            }
        }
    }

    #[test]
    fn stage2_leaves_small_history_alone() {
        let history: Vec<Message> = (0..2).map(|i| user_text(&format!("msg {i}"))).collect();
        let settings = settings_low_thresholds();
        let (_, freed) = stage2_snip(history, &settings);
        assert_eq!(freed, 0);
    }

    #[test]
    fn stage3_compacts_middle_only() {
        // Build: head(1) + many middle tool results + tail(2)
        let mut history = vec![user_text("system")];
        for i in 0..8 {
            history.push(tool_result_msg(&format!("t{i}"), &"y".repeat(300)));
        }
        history.push(user_text("recent1"));
        history.push(user_text("recent2"));

        let settings = ContextShaperSettings {
            snip_keep_head: 1,
            snip_keep_tail: 2,
            ..settings_low_thresholds()
        };
        let (out, freed) = stage3_micro_compact(history, &settings);
        assert!(freed > 0);
        // Head and tail should be untouched.
        assert_eq!(out.len(), 11);
    }

    #[tokio::test]
    async fn shape_context_no_change_when_small() {
        let history = vec![user_text("short")];
        let settings = ContextShaperSettings::default();
        let shaped = shape_context(history.clone(), &settings, None).await;
        assert!(shaped.stage_applied.is_none());
        assert_eq!(shaped.chars_freed, 0);
    }
}
