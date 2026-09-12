//! Per-message cache for the adapted transcript (AGE-165).
//!
//! `ChatView::render` rebuilds the typed transcript from `DisplayMessage`s on
//! every `cx.notify()`, and a streaming turn notifies once per coalesced text
//! batch (`FLUSH_INTERVAL` = 20ms, AGE-166). Adapting a message is not a cheap
//! copy: [`adapt_message_with_trace`] clones every tool block, parses tool
//! output as JSON for table previews and produced paths, and `stat`s those
//! paths to decide whether an artifact card is real. Doing that for the whole
//! history once per notify costs O(history) where the change is O(1).
//!
//! [`TurnCache`] keeps the adapted turns between frames and re-adapts only the
//! messages whose [`adapt_key`] moved — in a stream, the one turn being
//! written to.
//!
//! # The to-do panel is ownership, not an edit
//!
//! The adapter emits a [`Block::Plan`] on every turn that called `write_todos`,
//! but all of them render the same conversation-level snapshot, so only the
//! newest may keep it; and when no turn called the tool at all the panel is
//! grafted onto the last assistant turn. That used to be two passes that edited
//! the freshly built vec (`retain_last_plan_block` + `attach_plan_block`) —
//! destructive edits, which a cache would carry into the next frame and never
//! undo. Here the same rule is expressed as *which turn owns the panel*
//! ([`plan_owner`]) and re-asserted against whatever the cache holds
//! ([`apply_plan_ownership`]), so a cached turn converges on the same blocks a
//! full rebuild would have produced.

use std::hash::{Hash, Hasher};
use std::rc::Rc;

use rustc_hash::FxHasher;

use chatty_core::models::message_types::{SystemTrace, ToolCallState, ToolSource, TraceItem};

use crate::chatty::views::message_component::{DisplayMessage, MessageRole};
use crate::chatty::views::transcript::{Block, Turn, TurnRole, attach_plan_block};

/// Adapted turns from the last frame, plus what it took to decide they are
/// still current.
#[derive(Default)]
pub(super) struct TurnCache {
    /// The frame's turns. Shared rather than cloned — `render_message_list`
    /// and `render_turn` both read it.
    turns: Rc<Vec<Turn>>,
    /// [`adapt_key`] per turn, as of the last adapt.
    keys: Vec<u64>,
    /// Whether each turn's *own* trace produced a plan block. Recorded before
    /// ownership moves the panel, because ownership strips it off the losers
    /// and the cache would otherwise forget the turn ever had one.
    emits_plan: Vec<bool>,
}

impl std::ops::Deref for TurnCache {
    type Target = [Turn];

    fn deref(&self) -> &Self::Target {
        &self.turns
    }
}

impl TurnCache {
    /// The frame's turns, for a caller that needs to hold them past the borrow.
    pub(super) fn shared(&self) -> Rc<Vec<Turn>> {
        self.turns.clone()
    }

    /// Drop everything. For a conversation switch, where no turn of the old
    /// transcript describes the new one.
    pub(super) fn clear(&mut self) {
        self.turns = Rc::new(Vec::new());
        self.keys.clear();
        self.emits_plan.clear();
    }

    /// Bring the cache up to date with `keys`, calling `adapt` only for the
    /// entries that moved. Returns how many turns were adapted.
    ///
    /// `adapt` must produce the turn for that message index exactly as a full
    /// rebuild would, plan block included; ownership is applied afterwards.
    pub(super) fn refresh(
        &mut self,
        keys: &[u64],
        plan_active: bool,
        mut adapt: impl FnMut(usize) -> Turn,
    ) -> usize {
        // `make_mut` would deep-copy the whole transcript instead of editing
        // it, silently undoing the cache, if anything still held last frame's
        // handle. Nothing should: `render_message_list`'s clone dies with the
        // frame and `render_turn` copies one turn out, never the `Rc`.
        debug_assert_eq!(
            Rc::strong_count(&self.turns),
            1,
            "a stale handle on the frame's turns turns the cache into a full copy",
        );
        let turns = Rc::make_mut(&mut self.turns);
        // A torn cache is not worth repairing entry by entry.
        if turns.len() != self.keys.len() || turns.len() != self.emits_plan.len() {
            turns.clear();
            self.keys.clear();
            self.emits_plan.clear();
        }
        turns.truncate(keys.len());
        self.keys.truncate(keys.len());
        self.emits_plan.truncate(keys.len());

        let mut adapted = 0;
        for (index, key) in keys.iter().enumerate() {
            if self.keys.get(index) == Some(key) {
                continue;
            }
            let turn = adapt(index);
            let emits_plan = has_plan_block(&turn);
            if index < turns.len() {
                turns[index] = turn;
                self.keys[index] = *key;
                self.emits_plan[index] = emits_plan;
            } else {
                turns.push(turn);
                self.keys.push(*key);
                self.emits_plan.push(emits_plan);
            }
            adapted += 1;
        }

        let owner = plan_owner(turns, &self.emits_plan, plan_active);
        // A turn getting its *own* panel back is rebuilt, not patched. The
        // adapter puts the panel where `write_todos` ran in the trace, which
        // can be after a run of tool rows; `attach_plan_block` only knows how
        // to graft one onto the front. Reachable when the newer planning turn
        // goes away — a regenerate, or a reaped delegation row.
        if let Some(index) = owner
            && self.emits_plan[index]
            && !has_plan_block(&turns[index])
        {
            turns[index] = adapt(index);
            adapted += 1;
        }
        apply_plan_ownership(turns, owner);
        adapted
    }
}

/// Hash of everything [`adapt_message_with_trace`] reads for one message.
///
/// Lengths and discriminants, never payload bytes: this runs for every message
/// every frame, and the whole point is that a frame costs O(changed turns)
/// rather than O(history). Streaming text is append-only so its length always
/// moves, and a tool's payload only ever lands together with a state change —
/// the one field hashed by value is a tool error, since the message is rendered
/// and two failures can be the same length.
///
/// `index` is hashed because it is an adapt *input*, not just the slot the
/// result lands in — it becomes the turn's id and the namespace of every
/// `BlockId` under it. The cache happens to keep slot == index today, so this
/// never changes a decision; it is here so the key still describes the turn if
/// that ever stops being true.
///
/// `FxHasher`, not `DefaultHasher`: this runs for every message on every
/// notify, nothing hashed here is attacker-chosen, and the key is compared,
/// never used to place anything in a table — SipHash's keyed mixing was 68% of
/// the cached frame's self-time in AGE-165's profile (AGE-375).
///
/// [`adapt_message_with_trace`]: crate::chatty::views::transcript::adapt_message_with_trace
pub(super) fn adapt_key(
    msg: &DisplayMessage,
    index: usize,
    collapsed: bool,
    trace: Option<&SystemTrace>,
) -> u64 {
    let mut hasher = FxHasher::default();
    index.hash(&mut hasher);
    role_tag(&msg.role).hash(&mut hasher);
    msg.content.len().hash(&mut hasher);
    msg.is_streaming.hash(&mut hasher);
    // Paths, not just the count: `drop_artifact_cards_shown_inline` compares
    // them against the turn's artifact cards.
    msg.attachments.hash(&mut hasher);
    collapsed.hash(&mut hasher);

    let Some(trace) = trace else {
        return hasher.finish();
    };
    trace.items.len().hash(&mut hasher);
    trace.total_duration.hash(&mut hasher);
    trace.active_tool_index.hash(&mut hasher);
    for item in &trace.items {
        std::mem::discriminant(item).hash(&mut hasher);
        match item {
            TraceItem::Thinking(block) => {
                block.content.len().hash(&mut hasher);
                block.summary.len().hash(&mut hasher);
                block.duration.hash(&mut hasher);
                std::mem::discriminant(&block.state).hash(&mut hasher);
            }
            TraceItem::ToolCall(tool) => {
                tool.id.hash(&mut hasher);
                // By value: the name picks the block type (plan, diff, table
                // preview, artifact card, plain activity row).
                tool.tool_name.hash(&mut hasher);
                tool.display_name.len().hash(&mut hasher);
                tool.input.len().hash(&mut hasher);
                tool.output.as_ref().map(String::len).hash(&mut hasher);
                tool.output_preview
                    .as_ref()
                    .map(String::len)
                    .hash(&mut hasher);
                tool.duration.hash(&mut hasher);
                tool.text_before.len().hash(&mut hasher);
                match &tool.state {
                    ToolCallState::Running => 0u8.hash(&mut hasher),
                    ToolCallState::Success => 1u8.hash(&mut hasher),
                    ToolCallState::Error(message) => {
                        2u8.hash(&mut hasher);
                        message.hash(&mut hasher);
                    }
                }
                match &tool.source {
                    ToolSource::Local => 0u8.hash(&mut hasher),
                    ToolSource::HiveCloud => 1u8.hash(&mut hasher),
                    ToolSource::Internet { label } => {
                        2u8.hash(&mut hasher);
                        label.hash(&mut hasher);
                    }
                    ToolSource::ExternalService { name } => {
                        3u8.hash(&mut hasher);
                        name.hash(&mut hasher);
                    }
                }
                tool.execution_engine
                    .as_ref()
                    .map(std::mem::discriminant)
                    .hash(&mut hasher);
            }
            TraceItem::ApprovalPrompt(approval) => {
                approval.id.hash(&mut hasher);
                approval.command.len().hash(&mut hasher);
                approval.is_sandboxed.hash(&mut hasher);
                std::mem::discriminant(&approval.state).hash(&mut hasher);
            }
            TraceItem::ClarificationPrompt(clarification) => {
                clarification.id.hash(&mut hasher);
                clarification.questions.len().hash(&mut hasher);
                clarification.answers.len().hash(&mut hasher);
                std::mem::discriminant(&clarification.state).hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn role_tag(role: &MessageRole) -> u8 {
    match role {
        MessageRole::User => 0,
        MessageRole::Assistant => 1,
    }
}

fn has_plan_block(turn: &Turn) -> bool {
    turn.blocks
        .iter()
        .any(|block| matches!(block, Block::Plan { .. }))
}

/// Which turn draws the to-do panel.
///
/// The newest turn that planned for itself, otherwise the last assistant turn
/// while a snapshot is live, otherwise nobody.
fn plan_owner(turns: &[Turn], emits_plan: &[bool], plan_active: bool) -> Option<usize> {
    if let Some(index) = emits_plan.iter().rposition(|emits| *emits) {
        return Some(index);
    }
    if !plan_active {
        return None;
    }
    turns
        .iter()
        .rposition(|turn| matches!(turn.role, TurnRole::Assistant))
}

/// Put the panel on `owner` and take it off everyone else.
///
/// Idempotent, which is what lets a cached turn be left alone: a turn already
/// in the right state is untouched.
fn apply_plan_ownership(turns: &mut [Turn], owner: Option<usize>) {
    for (index, turn) in turns.iter_mut().enumerate() {
        let wanted = owner == Some(index);
        match (wanted, has_plan_block(turn)) {
            (true, false) => attach_plan_block(std::slice::from_mut(turn), true),
            (false, true) => turn
                .blocks
                .retain(|block| !matches!(block, Block::Plan { .. })),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatty::views::transcript::adapt_message_with_trace;
    use chatty_core::models::message_types::ToolCallBlock;
    use std::time::{Duration, Instant};

    fn message(role: MessageRole, content: &str, trace: Option<SystemTrace>) -> DisplayMessage {
        DisplayMessage {
            role,
            content: content.to_string(),
            is_streaming: false,
            system_trace_view: None,
            live_trace: trace,
            is_markdown: true,
            attachments: Vec::new(),
            feedback: None,
            history_index: None,
        }
    }

    fn tool(id: &str, name: &str) -> ToolCallBlock {
        ToolCallBlock {
            id: id.into(),
            tool_name: name.into(),
            display_name: name.into(),
            input: r#"{"path":"notes.md"}"#.into(),
            output: Some("ok".into()),
            output_preview: None,
            state: ToolCallState::Success,
            duration: Some(Duration::from_millis(12)),
            text_before: String::new(),
            source: ToolSource::Local,
            execution_engine: None,
        }
    }

    fn trace_of(tools: Vec<ToolCallBlock>) -> SystemTrace {
        SystemTrace {
            items: tools.into_iter().map(TraceItem::ToolCall).collect(),
            total_duration: Some(Duration::from_secs(1)),
            active_tool_index: None,
        }
    }

    /// A file that is really on disk, so an artifact receipt survives the
    /// adapter's `produced_path_is_openable` check — the `stat` per tool call
    /// per message per frame is a good part of what this cache is for.
    fn written_file() -> &'static std::path::Path {
        static PATH: std::sync::LazyLock<std::path::PathBuf> = std::sync::LazyLock::new(|| {
            let dir = std::env::temp_dir().join("chatty_turn_cache_bench");
            std::fs::create_dir_all(&dir).expect("temp dir");
            let path = dir.join("report.md");
            std::fs::write(&path, b"# report").expect("write file");
            path
        });
        PATH.as_path()
    }

    /// A long history with a streaming tail, as the hot path sees it: prose,
    /// a couple of plain tool rows, and one call that mints an artifact card.
    fn history(turns: usize) -> Vec<DisplayMessage> {
        let mut messages = Vec::new();
        for index in 0..turns {
            messages.push(message(MessageRole::User, &format!("ask {index}"), None));
            let mut produced = tool(&format!("t{index}c"), "write_file");
            produced.input = format!(
                r#"{{"path":"{}","content":"{}"}}"#,
                written_file().display(),
                "x".repeat(400),
            );
            produced.output = Some(format!(
                r#"{{"saved_path":"{}"}}"#,
                written_file().display()
            ));
            messages.push(message(
                MessageRole::Assistant,
                &"answer ".repeat(120),
                Some(trace_of(vec![
                    tool(&format!("t{index}a"), "read_file"),
                    tool(&format!("t{index}b"), "search_files"),
                    produced,
                ])),
            ));
        }
        messages
    }

    fn keys_for(messages: &[DisplayMessage]) -> Vec<u64> {
        messages
            .iter()
            .enumerate()
            .map(|(index, msg)| adapt_key(msg, index, false, msg.live_trace.as_ref()))
            .collect()
    }

    fn refresh(
        cache: &mut TurnCache,
        messages: &[DisplayMessage],
        plan_active: bool,
        adapts: &mut usize,
    ) {
        let keys = keys_for(messages);
        *adapts += cache.refresh(&keys, plan_active, |index| {
            let msg = &messages[index];
            adapt_message_with_trace(msg, index, false, msg.live_trace.as_ref())
        });
    }

    /// The acceptance criterion, as a check: a frame costs one adapt per
    /// changed turn, not one per message.
    #[test]
    fn a_streaming_frame_adapts_only_the_turn_that_changed() {
        let mut messages = history(60);
        let mut cache = TurnCache::default();
        let mut adapts = 0;
        refresh(&mut cache, &messages, false, &mut adapts);
        assert_eq!(adapts, messages.len(), "the first frame adapts everything");

        for _ in 0..25 {
            adapts = 0;
            messages.last_mut().expect("tail").content.push_str("more ");
            refresh(&mut cache, &messages, false, &mut adapts);
            assert_eq!(adapts, 1, "a text chunk touches one turn");
        }
    }

    /// The cache must never show a turn a full rebuild would not have produced.
    #[test]
    fn a_cached_frame_matches_a_full_rebuild() {
        let mut messages = history(8);
        let mut cache = TurnCache::default();
        let mut fresh = TurnCache::default();
        let mut adapts = 0;

        // The mutations the hot path really performs, in the order that makes
        // the plan panel move: stream, ask again, plan, re-plan, regenerate
        // (drop the tail turn), finalize (live trace → persisted).
        #[derive(Clone, Copy)]
        enum Step {
            Stream,
            Ask,
            Plan,
            RePlan,
            DropTail,
            Finalize,
        }

        let steps = [
            Step::Stream,
            Step::Ask,
            Step::Plan,
            Step::RePlan,
            Step::DropTail,
            Step::Finalize,
        ];

        for (step, mutation) in steps.iter().enumerate() {
            for plan_active in [false, true] {
                match mutation {
                    Step::Stream => messages.last_mut().expect("tail").content.push('x'),
                    Step::Ask => messages.push(message(MessageRole::User, "next", None)),
                    // A tool row ahead of the plan call, so the panel does
                    // not sit at block 0 — that is where a cached turn that
                    // regains the panel would wrongly put it back.
                    Step::Plan => messages.push(message(
                        MessageRole::Assistant,
                        "planning",
                        Some(trace_of(vec![
                            tool("r1", "read_file"),
                            tool("p1", "write_todos"),
                        ])),
                    )),
                    Step::RePlan => messages.push(message(
                        MessageRole::Assistant,
                        "replanning",
                        Some(trace_of(vec![
                            tool("r2", "read_file"),
                            tool("p2", "write_todos"),
                        ])),
                    )),
                    Step::DropTail => {
                        messages.truncate(messages.len() - 1);
                    }
                    Step::Finalize => messages.last_mut().expect("tail").live_trace = None,
                }
                refresh(&mut cache, &messages, plan_active, &mut adapts);
                fresh.clear();
                refresh(&mut fresh, &messages, plan_active, &mut adapts);
                assert_eq!(
                    format!("{:?}", &*cache),
                    format!("{:?}", &*fresh),
                    "step {step} (plan_active={plan_active}) diverged from a full rebuild"
                );
            }
        }
    }

    /// Only the newest planning turn keeps the panel, cached or not.
    #[test]
    fn the_panel_moves_to_the_newest_planning_turn() {
        let messages = vec![
            message(
                MessageRole::Assistant,
                "first plan",
                Some(trace_of(vec![tool("p1", "write_todos")])),
            ),
            message(MessageRole::User, "again", None),
            message(
                MessageRole::Assistant,
                "second plan",
                Some(trace_of(vec![tool("p2", "write_todos")])),
            ),
        ];
        let mut cache = TurnCache::default();
        let mut adapts = 0;
        refresh(&mut cache, &messages, true, &mut adapts);
        let owners: Vec<bool> = cache.iter().map(has_plan_block).collect();
        assert_eq!(owners, vec![false, false, true]);
    }

    /// The grafted panel is taken off again once the snapshot is cleared —
    /// the case a cache that only ever *added* blocks would get wrong.
    #[test]
    fn clearing_the_snapshot_removes_a_grafted_panel() {
        let messages = vec![
            message(MessageRole::User, "hi", None),
            message(MessageRole::Assistant, "working", Some(trace_of(vec![]))),
        ];
        let mut cache = TurnCache::default();
        let mut adapts = 0;
        refresh(&mut cache, &messages, true, &mut adapts);
        assert!(cache.iter().any(has_plan_block), "a live snapshot draws");

        refresh(&mut cache, &messages, false, &mut adapts);
        assert!(
            !cache.iter().any(has_plan_block),
            "a cleared snapshot leaves no panel behind"
        );
    }

    #[test]
    fn a_tool_finishing_changes_the_key() {
        let mut running = tool("t", "read_file");
        running.state = ToolCallState::Running;
        running.output = None;
        let mut done = running.clone();
        done.state = ToolCallState::Success;
        done.output = Some("contents".into());

        let before = message(MessageRole::Assistant, "", Some(trace_of(vec![running])));
        let after = message(MessageRole::Assistant, "", Some(trace_of(vec![done])));
        assert_ne!(
            adapt_key(&before, 0, false, before.live_trace.as_ref()),
            adapt_key(&after, 0, false, after.live_trace.as_ref())
        );
    }

    #[test]
    fn two_failures_of_the_same_length_differ() {
        let mut first = tool("t", "shell");
        first.state = ToolCallState::Error("no such file".into());
        let mut second = first.clone();
        second.state = ToolCallState::Error("no such host".into());

        let a = message(MessageRole::Assistant, "", Some(trace_of(vec![first])));
        let b = message(MessageRole::Assistant, "", Some(trace_of(vec![second])));
        assert_ne!(
            adapt_key(&a, 0, false, a.live_trace.as_ref()),
            adapt_key(&b, 0, false, b.live_trace.as_ref())
        );
    }

    #[test]
    fn folding_a_turn_changes_the_key() {
        let msg = message(MessageRole::Assistant, "answer", Some(trace_of(vec![])));
        assert_ne!(
            adapt_key(&msg, 0, false, msg.live_trace.as_ref()),
            adapt_key(&msg, 0, true, msg.live_trace.as_ref())
        );
    }

    #[test]
    fn an_unchanged_message_keeps_its_key() {
        let msg = message(
            MessageRole::Assistant,
            "answer",
            Some(trace_of(vec![tool("t", "read_file")])),
        );
        assert_eq!(
            adapt_key(&msg, 0, false, msg.live_trace.as_ref()),
            adapt_key(&msg, 0, false, msg.live_trace.as_ref())
        );
    }

    /// Wall-clock evidence for AGE-165. Not a check — a measurement, so it is
    /// `#[ignore]`d and must be run in release:
    ///
    /// ```text
    /// cargo test --release -p chatty-gpui --all-features \
    ///     turn_cache::tests::adapt_cost -- --ignored --nocapture
    /// ```
    ///
    /// `samply` and `cargo-flamegraph` are not installable in this sandbox and
    /// `perf_event_paranoid` blocks unprivileged sampling, so the self-time of
    /// the adapt is timed directly instead of read off a flamegraph.
    #[test]
    #[ignore = "measurement, not a check; run --release with --ignored --nocapture"]
    fn adapt_cost_under_a_long_history_and_a_fast_stream() {
        const FRAMES: usize = 200;
        let mut messages = history(100);
        let collapsed = vec![false; messages.len()];

        // Before: every notify re-adapted the whole history (and, until the
        // virtual list was retired, twice per frame).
        let started = Instant::now();
        for _ in 0..FRAMES {
            messages
                .last_mut()
                .expect("tail")
                .content
                .push_str("chunk ");
            let traces: Vec<Option<SystemTrace>> =
                messages.iter().map(|m| m.live_trace.clone()).collect();
            let turns = crate::chatty::views::transcript::adapt_messages_with_traces(
                &messages, &collapsed, &traces,
            );
            std::hint::black_box(&turns);
        }
        let full = started.elapsed();

        // After: the same frames through the cache.
        let mut cache = TurnCache::default();
        let mut adapts = 0;
        refresh(&mut cache, &messages, false, &mut adapts);
        adapts = 0;
        let started = Instant::now();
        for _ in 0..FRAMES {
            messages
                .last_mut()
                .expect("tail")
                .content
                .push_str("chunk ");
            refresh(&mut cache, &messages, false, &mut adapts);
            std::hint::black_box(&*cache);
        }
        let cached = started.elapsed();

        println!(
            "AGE-165 adapt self-time over {FRAMES} frames of a {}-message history\n  \
             full rebuild per frame: {:>9.3?}  ({:>8.3?}/frame)\n  \
             cached (this change):   {:>9.3?}  ({:>8.3?}/frame)\n  \
             speedup: {:.1}x   turns adapted: {adapts} of {} possible",
            messages.len(),
            full,
            full / FRAMES as u32,
            cached,
            cached / FRAMES as u32,
            full.as_secs_f64() / cached.as_secs_f64().max(f64::MIN_POSITIVE),
            messages.len() * FRAMES,
        );
        assert_eq!(adapts, FRAMES, "one adapt per frame, not one per message");
    }
}
