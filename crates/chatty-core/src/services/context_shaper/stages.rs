//! The shaping stages: terminal-context and dragged-file stubbing, cap, compact, snip and trim,
//! the round-trip-safe cut points they share, and the request assembly that
//! applies them (with the compaction and the results ledger) before a call.

use std::collections::HashSet;

use rig_core::completion::Message;
use rig_core::completion::message::{CallId, Text, ToolResult, ToolResultContent};
use rig_core::message::UserContent;
use tracing::{debug, warn};

use crate::services::composer_context::stub_older_file_contexts;
use crate::services::results_ledger;
use crate::services::terminal::context::stub_older_contexts;
use crate::services::{call_ids, enforce_tool_round_trips, result_ids, tool_round_trips_intact};

use super::{ContextShaper, ContextShaperStage, ShapedContext};

/// Tool result payloads larger than this are stubbed by the cap stages.
pub(super) const TOOL_RESULT_CAP_BYTES: usize = 8_192;

/// Messages kept whole at the head of the history when snipping.
pub(super) const KEEP_HEAD: usize = 2;

/// Messages kept whole at the tail of the history: what the model is working
/// with right now.
pub(super) const KEEP_TAIL: usize = 8;
/// The compact stages keep this many characters of a tool result.
pub(super) const COMPACT_PREVIEW_CHARS: usize = 120;

impl ContextShaper {
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
        self.request_history_reserving(history, prompt, turn, 0)
    }

    /// [`Self::request_history`] with `reserve` more tokens kept free, for
    /// what the guard adds to the request itself (the results ledger).
    pub(super) fn request_history_reserving(
        &self,
        history: &[Message],
        prompt: &Message,
        turn: usize,
        reserve: usize,
    ) -> Option<Vec<Message>> {
        let prompt_tokens = self.inner.counter.count_message(prompt) + reserve;
        // rig hands the hook the prompt separately from the history, and
        // mid-tool-loop that prompt is the result answering the history's
        // last call — a call it answers is not a dangling one (AGE-513).
        let answered_by_prompt: Vec<CallId> = result_ids(prompt).cloned().collect();

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

    /// What the completion-call hook sends: [`Self::request_history`] over
    /// the compacted history when there is a compaction in force (or one is
    /// due now), over the recorded one otherwise — plus the results ledger
    /// when that request no longer shows every delegation's result verbatim
    /// (AGE-941). `None` when the recorded history goes out as is.
    pub async fn prepare_request(
        &self,
        history: &[Message],
        prompt: &Message,
        turn: usize,
    ) -> Option<Vec<Message>> {
        self.prepare_request_reserving(history, prompt, turn, 0)
            .await
    }

    /// [`Self::prepare_request`] with `extra_reserve` more tokens kept free
    /// for the reply ([`ReplyRoom`]).
    pub async fn prepare_request_reserving(
        &self,
        history: &[Message],
        prompt: &Message,
        turn: usize,
        extra_reserve: usize,
    ) -> Option<Vec<Message>> {
        let prompt_tokens = self.inner.counter.count_message(prompt) + extra_reserve;
        let compacted = self.compacted(history, prompt_tokens, turn).await;
        let base = compacted.as_deref().unwrap_or(history);
        let sent = match self.request_history_reserving(base, prompt, turn, extra_reserve) {
            Some(sent) => sent,
            None => compacted.clone()?,
        };
        Some(self.with_results_ledger(history, base, prompt, turn, sent, extra_reserve))
    }

    /// `sent` with the results ledger ([`results_ledger`]) right after its
    /// head when it no longer carries every delegation result of `history`
    /// verbatim; `sent` unchanged otherwise. Room for the ledger is made by
    /// shaping `base` again with the ledger counted against the budget.
    pub(super) fn with_results_ledger(
        &self,
        history: &[Message],
        base: &[Message],
        prompt: &Message,
        turn: usize,
        sent: Vec<Message>,
        extra_reserve: usize,
    ) -> Vec<Message> {
        let total = results_ledger::delegations(history).len();
        if total == 0 {
            return sent;
        }
        let prompt_tokens = self.inner.counter.count_message(prompt) + extra_reserve;
        let max_chars = results_ledger::max_chars(self.history_budget(prompt_tokens));
        let ledger_for = |sent: &[Message]| {
            results_ledger::ledger_message(
                &results_ledger::not_shown(history, sent),
                total,
                max_chars,
            )
        };
        let Some(mut ledger) = ledger_for(&sent) else {
            return sent;
        };
        // A tighter budget can take one more result out, and the ledger
        // grows with it: shape again until the room kept covers the ledger
        // (it is capped, so this settles at once in practice).
        let mut sent = sent;
        for _ in 0..3 {
            let reserve = self.inner.counter.count_message(&ledger) + extra_reserve;
            sent = self
                .request_history_reserving(base, prompt, turn, reserve)
                .unwrap_or_else(|| base.to_vec());
            let Some(next) = ledger_for(&sent) else {
                return sent;
            };
            let settled = self.inner.counter.count_message(&next) <= reserve;
            ledger = next;
            if settled {
                break;
            }
        }
        debug!(
            turn,
            delegations = total,
            "context shaper: results ledger added to the request"
        );
        let at = round_trip_safe_head(&sent, self.inner.settings.keep_head);
        sent.insert(at, ledger);
        sent
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

        // Stage 0: every terminal snapshot but the newest (AGE-587). The
        // screen it showed has moved on; the newest one says where it is.
        let mut stage = ContextShaperStage::TerminalContext;
        // Files dragged into older messages go with them (AGE-2169).
        let mut stubbed = stub_older_contexts(&mut messages);
        stubbed.extend(stub_older_file_contexts(&mut messages));
        for i in stubbed {
            counts[i] = counter.count_message(&messages[i]);
        }

        // Stage 1: cap oversized tool results outside the tail.
        if !fits(&counts) {
            stage = ContextShaperStage::Cap;
            for i in 0..tail_start {
                if cap_tool_results(&mut messages[i], settings.tool_result_cap_bytes) {
                    counts[i] = counter.count_message(&messages[i]);
                }
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
        // the other (AGE-512). The latest user message stays, right after
        // the marker: mid-run it is what the run is answering (AGE-994).
        let mut snip: Option<Snip> = None;
        if !fits(&counts) && len > settings.keep_head + settings.keep_tail + 1 {
            let head = round_trip_safe_head(&messages, settings.keep_head);
            let snip_end = round_trip_safe_tail_start(&messages, head, tail_start);
            let kept_user = latest_user_message(&messages).filter(|i| (head..snip_end).contains(i));
            if snip_end > head + usize::from(kept_user.is_some()) {
                stage = ContextShaperStage::Snip;
                let dropped = snip_end - head - usize::from(kept_user.is_some());
                let marker = snip_marker(dropped);
                let marker_tokens = counter.count_message(&marker);
                let tail = messages.split_off(snip_end);
                let tail_counts = counts.split_off(snip_end);
                let kept_user = kept_user.map(|i| (messages[i].clone(), counts[i]));
                messages.truncate(head);
                counts.truncate(head);
                messages.push(marker);
                counts.push(marker_tokens);
                if let Some((message, tokens)) = kept_user {
                    messages.push(message);
                    counts.push(tokens);
                }
                messages.extend(tail);
                counts.extend(tail_counts);
                snip = Some(Snip { at: head, dropped });
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

        // Stage 5: drop the kept tail's messages too, oldest first (AGE-994).
        // What is left over budget here is text no stage shrinks — earlier
        // turns' replies, the code a tool call carries — and at turn start
        // that is most of a long conversation. A call goes with the results
        // answering it, so no round-trip is split; the latest user message
        // and the last message stay.
        if !fits(&counts) {
            let (at, mut dropped) = match snip {
                Some(Snip { at, dropped }) => (at, dropped),
                None => {
                    let at =
                        round_trip_safe_head(&messages, settings.keep_head.min(messages.len()));
                    let marker = snip_marker(0);
                    counts.insert(at, counter.count_message(&marker));
                    messages.insert(at, marker);
                    (at, 0)
                }
            };
            let snipped = dropped;
            let mut protected = latest_user_message(&messages).filter(|i| *i > at);
            let mut i = at + 1;
            while !fits(&counts) && i + 1 < messages.len() {
                if protected == Some(i) {
                    i += 1;
                    continue;
                }
                let end = round_trip_end(&messages, i);
                if end >= messages.len() {
                    break;
                }
                messages.drain(i..end);
                counts.drain(i..end);
                dropped += end - i;
                protected = protected.map(|p| if p > i { p - (end - i) } else { p });
            }
            if dropped > snipped {
                stage = ContextShaperStage::Trim;
                messages[at] = snip_marker(dropped);
                counts[at] = counter.count_message(&messages[at]);
            } else if snip.is_none() {
                messages.remove(at);
                counts.remove(at);
            }
        }

        let tokens_after = counts.iter().sum();
        if tokens_after > budget {
            warn!(
                tokens = tokens_after,
                budget, "context shaper: history is still over budget after every stage"
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

// ── Snipping ──────────────────────────────────────────────────────────────────

/// Where the snip marker stands in a shaped history, and how many messages
/// it stands for.
#[derive(Clone, Copy)]
struct Snip {
    at: usize,
    dropped: usize,
}

/// The one message standing where `dropped` messages were.
fn snip_marker(dropped: usize) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text::new(format!(
            "[CONTEXT SHAPER: {dropped} messages snipped to reduce context size]"
        )))],
    }
}

/// The index of the newest user message that carries text and no tool
/// result: what a person, or the run's own protocol, last asked.
pub(super) fn latest_user_message(messages: &[Message]) -> Option<usize> {
    messages.iter().rposition(|message| {
        let Message::User { content } = message else {
            return false;
        };
        content
            .iter()
            .any(|item| matches!(item, UserContent::Text(_)))
            && !content
                .iter()
                .any(|item| matches!(item, UserContent::ToolResult(_)))
    })
}

/// The end of the round-trip starting at `start`: the message itself and,
/// when it makes tool calls, the result messages right after it that
/// answer them.
fn round_trip_end(messages: &[Message], start: usize) -> usize {
    let calls: HashSet<&CallId> = call_ids(&messages[start]).collect();
    let mut end = start + 1;
    if calls.is_empty() {
        return end;
    }
    while end < messages.len()
        && result_ids(&messages[end]).next().is_some()
        && result_ids(&messages[end]).all(|id| calls.contains(id))
    {
        end += 1;
    }
    end
}

// ── Round trips ───────────────────────────────────────────────────────────────

/// Pull the head of the snip back off an assistant `tool_calls` message whose
/// results the snip would drop: a call nothing answers is a 400 from every
/// OpenAI-compatible provider, the same one AGE-485 repaired at the other end.
fn round_trip_safe_head(messages: &[Message], mut head: usize) -> usize {
    while head > 0 {
        let answered: HashSet<&CallId> = messages[..head].iter().flat_map(result_ids).collect();
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
pub(super) fn round_trip_safe_tail_start(
    messages: &[Message],
    head: usize,
    tail_start: usize,
) -> usize {
    let mut start = tail_start;
    while start > head {
        let called: HashSet<&CallId> = messages[..head]
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
pub(super) fn cap_tool_results(message: &mut Message, cap: usize) -> bool {
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
pub(super) fn compact_tool_results(message: &mut Message) -> bool {
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
