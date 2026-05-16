//! Agent loop guard — shared across frontends (TUI and GPUI).
//!
//! Detects two classes of runaway agent behavior and returns corrective
//! injection prompts that the frontend can send as the next user message:
//!
//! 1. **Repeated tool calls** — the model calls the same tool with identical
//!    arguments two or more times in a row. A pivot prompt is returned so the
//!    agent tries a different approach instead of spinning.
//!
//! 2. **Late-game stall** — when the agent is within `LATE_GAME_THRESHOLD`
//!    turns of the per-conversation limit and still has not produced an answer
//!    file, a deadline prompt is returned once to force a commit.
//!
//! 3. **Verbosity guard** — tracks how many bytes of plain text have been
//!    emitted in the current turn without a tool call. Returns `true` when the
//!    soft limit is exceeded so the frontend can stop the stream early.
//!
//! ## Usage (in both frontends)
//!
//! ```ignore
//! let mut guard = AgentLoopGuard::new(max_agent_turns, answer_file_required);
//!
//! // After each tool result:
//! if let Some(pivot) = guard.on_tool_completed(&name, &input) {
//!     engine.send_message(pivot);
//! }
//!
//! // At the end of each stream turn:
//! guard.on_turn_complete(turns_used, has_answer);
//! if let Some(deadline) = guard.deadline_message() {
//!     engine.send_message(deadline.to_string());
//! }
//!
//! // During TextChunk handling:
//! if guard.on_text_chunk(chunk.len()) {
//!     engine.stop_stream(); // verbosity limit exceeded
//! }
//! ```

use std::collections::VecDeque;

// ── Tunables ──────────────────────────────────────────────────────────────────

/// How many bytes of pure text (no tool call) in one turn triggers the soft
/// verbosity stop.  Kept lower than the TUI hard-stop constant so frontends
/// get an early warning rather than waiting for an 8 KB runaway.
const VERBOSITY_SOFT_LIMIT_BYTES: usize = 4_000;

/// Maximum number of loop-pivot injections per agent session.  After this the
/// guard stops firing so we don't loop on the pivot itself.
const MAX_LOOP_PIVOTS: usize = 3;

/// How many turns before the hard cap to inject the deadline prompt.
const LATE_GAME_THRESHOLD: usize = 2;

/// Ring-buffer size for recent tool calls used for repetition detection.
const TOOL_CALL_HISTORY_LEN: usize = 6;

// ── Public types ──────────────────────────────────────────────────────────────

/// Stateful guard that detects and corrects runaway agent loops.
///
/// Construct one per agent invocation (per message send) and thread it through
/// the event loop.  Both `chatty-tui` (headless mode) and `chatty-gpui`
/// (desktop app) should use this so the behaviour is consistent.
pub struct AgentLoopGuard {
    /// Whether the benchmark task expects an answer file to be produced.
    answer_file_required: bool,

    /// Hard maximum number of agent turns for this session.
    max_agent_turns: usize,

    /// Ring buffer of `(tool_name, truncated_input)` for the last N tool calls.
    recent_tool_calls: VecDeque<(String, String)>,

    /// How many loop-pivot prompts have been injected so far.
    loop_pivot_count: usize,

    /// Whether the late-game deadline prompt has already been injected.
    late_game_injected: bool,

    /// Bytes of plain text emitted in the current turn (reset on `on_turn_complete`).
    text_bytes_this_turn: usize,

    /// Whether a tool was called in the current turn (resets verbosity counter).
    tool_called_this_turn: bool,

    /// Pending deadline message to be retrieved after `on_turn_complete`.
    pending_deadline: Option<String>,
}

impl AgentLoopGuard {
    /// Create a new guard for a single agent invocation.
    ///
    /// - `max_agent_turns`: the `execution_settings.max_agent_turns` value.
    /// - `answer_file_required`: true when the task prompt mentions `answer.txt`.
    pub fn new(max_agent_turns: usize, answer_file_required: bool) -> Self {
        Self {
            answer_file_required,
            max_agent_turns,
            recent_tool_calls: VecDeque::with_capacity(TOOL_CALL_HISTORY_LEN),
            loop_pivot_count: 0,
            late_game_injected: false,
            text_bytes_this_turn: 0,
            tool_called_this_turn: false,
            pending_deadline: None,
        }
    }

    // ── Event handlers ────────────────────────────────────────────────────────

    /// Call after each completed tool result (success or error).
    ///
    /// Returns a pivot message to inject if the same `(name, input)` pair has
    /// appeared at least twice consecutively in the recent history.
    pub fn on_tool_completed(&mut self, name: &str, input: &str) -> Option<String> {
        let entry = (name.to_string(), truncate_input(input));
        self.tool_called_this_turn = true;

        // Add to ring buffer.
        if self.recent_tool_calls.len() >= TOOL_CALL_HISTORY_LEN {
            self.recent_tool_calls.pop_front();
        }
        self.recent_tool_calls.push_back(entry);

        // Check if the last two entries are identical.
        if self.loop_pivot_count >= MAX_LOOP_PIVOTS {
            return None;
        }
        let len = self.recent_tool_calls.len();
        if len < 2 {
            return None;
        }
        let last = &self.recent_tool_calls[len - 1];
        let prev = &self.recent_tool_calls[len - 2];
        if last == prev {
            self.loop_pivot_count += 1;
            let input_preview = &last.1;
            Some(format!(
                "LOOP DETECTED: You just called `{name}` with the same arguments twice in a row \
                 (input: {input_preview}). That approach is not working. \
                 Switch to a completely different strategy — try a different tool, \
                 different search terms, a different API, or different computation method. \
                 Do NOT repeat the same call again."
            ))
        } else {
            None
        }
    }

    /// Call at the end of each stream turn (on `StreamCompleted`).
    ///
    /// - `turns_used`: number of assistant turns completed so far.
    /// - `has_answer`: whether the answer file already exists.
    ///
    /// After calling this, check [`Self::deadline_message`] to see if a
    /// deadline prompt should be injected.
    pub fn on_turn_complete(&mut self, turns_used: usize, has_answer: bool) {
        // Reset per-turn verbosity tracking.
        self.text_bytes_this_turn = 0;
        self.tool_called_this_turn = false;

        // Fire deadline at most once, only when answer is still missing.
        if self.late_game_injected
            || !self.answer_file_required
            || has_answer
            || self.max_agent_turns == 0
        {
            self.pending_deadline = None;
            return;
        }

        let remaining = self.max_agent_turns.saturating_sub(turns_used);
        if remaining <= LATE_GAME_THRESHOLD {
            self.late_game_injected = true;
            self.pending_deadline = Some(format!(
                "DEADLINE: Only {remaining} turn(s) remaining and required output is still missing. \
                 Provide your best final answer now instead of continuing to research. \
                 Use the available response tool/output channel immediately."
            ));
        } else {
            self.pending_deadline = None;
        }
    }

    /// Returns the pending deadline message set by the last `on_turn_complete`
    /// call, if any.  Consuming the message: caller should drain this after
    /// checking so it doesn't re-fire on the next call.
    pub fn take_deadline_message(&mut self) -> Option<String> {
        self.pending_deadline.take()
    }

    /// Call for each text chunk received from the LLM stream (before any tool
    /// call in the current turn).
    ///
    /// Returns `true` when the soft verbosity limit is exceeded — the frontend
    /// should inject a recovery prompt once the stream finishes. Callers that
    /// only want to react once per turn should de-duplicate the `true` result.
    pub fn on_text_chunk(&mut self, bytes: usize) -> bool {
        if self.tool_called_this_turn {
            return false;
        }
        self.text_bytes_this_turn += bytes;
        self.text_bytes_this_turn > VERBOSITY_SOFT_LIMIT_BYTES
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    /// Number of loop pivots injected so far (useful for logging).
    pub fn loop_pivot_count(&self) -> usize {
        self.loop_pivot_count
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Truncate and clean a tool input string for use in loop detection and
/// in pivot prompt messages.  We only need enough to distinguish two calls.
fn truncate_input(input: &str) -> String {
    let clean = input.trim();
    if clean.chars().count() <= 120 {
        clean.to_string()
    } else {
        let head: String = clean.chars().take(120).collect();
        format!("{head}…")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> AgentLoopGuard {
        AgentLoopGuard::new(20, true)
    }

    #[test]
    fn no_pivot_on_first_call() {
        let mut g = guard();
        assert!(g.on_tool_completed("search_web", "rust async").is_none());
    }

    #[test]
    fn no_pivot_when_calls_differ() {
        let mut g = guard();
        g.on_tool_completed("search_web", "rust async");
        assert!(
            g.on_tool_completed("search_web", "different query")
                .is_none()
        );
    }

    #[test]
    fn pivot_on_identical_consecutive_calls() {
        let mut g = guard();
        g.on_tool_completed("search_web", "rust async");
        let result = g.on_tool_completed("search_web", "rust async");
        assert!(result.is_some());
        let msg = result.unwrap();
        assert!(msg.contains("search_web"));
        assert!(msg.contains("LOOP DETECTED"));
    }

    #[test]
    fn pivot_capped_at_max() {
        let mut g = guard();
        for _ in 0..MAX_LOOP_PIVOTS {
            g.on_tool_completed("search_web", "q");
            g.on_tool_completed("search_web", "q");
        }
        // After MAX_LOOP_PIVOTS the guard returns None.
        let result = g.on_tool_completed("search_web", "q");
        assert!(result.is_none());
    }

    #[test]
    fn no_deadline_with_turns_remaining() {
        let mut g = guard();
        g.on_turn_complete(5, false);
        assert!(g.take_deadline_message().is_none());
    }

    #[test]
    fn deadline_fires_near_end() {
        let mut g = AgentLoopGuard::new(10, true);
        g.on_turn_complete(8, false); // 2 remaining
        assert!(g.take_deadline_message().is_some());
    }

    #[test]
    fn deadline_not_fired_when_answer_exists() {
        let mut g = AgentLoopGuard::new(10, true);
        g.on_turn_complete(8, true); // has_answer = true
        assert!(g.take_deadline_message().is_none());
    }

    #[test]
    fn deadline_fires_only_once() {
        let mut g = AgentLoopGuard::new(10, true);
        g.on_turn_complete(8, false);
        let first = g.take_deadline_message();
        g.on_turn_complete(9, false);
        let second = g.take_deadline_message();
        assert!(first.is_some());
        assert!(second.is_none()); // already fired
    }

    #[test]
    fn verbosity_ok_below_limit() {
        let mut g = guard();
        assert!(!g.on_text_chunk(100));
        assert!(!g.on_text_chunk(100));
    }

    #[test]
    fn verbosity_triggers_above_limit() {
        let mut g = guard();
        // Feed bytes over the limit
        let over = VERBOSITY_SOFT_LIMIT_BYTES + 1;
        assert!(g.on_text_chunk(over));
    }

    #[test]
    fn verbosity_suppressed_after_tool_call() {
        let mut g = guard();
        g.on_tool_completed("shell", "echo hi");
        // After a tool call, verbosity guard is disabled for this turn.
        assert!(!g.on_text_chunk(VERBOSITY_SOFT_LIMIT_BYTES + 1));
    }

    #[test]
    fn verbosity_resets_on_turn_complete() {
        let mut g = guard();
        g.on_text_chunk(VERBOSITY_SOFT_LIMIT_BYTES + 1);
        g.on_turn_complete(1, false);
        // Fresh turn: counter reset, no trigger until limit exceeded again.
        assert!(!g.on_text_chunk(100));
    }
}
