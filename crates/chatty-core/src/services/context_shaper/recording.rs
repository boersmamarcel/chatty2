//! Tool-output recording caps: a result larger than the history budget can
//! hold is cut once, at recording, so the same bytes go out on every request.

use rig_core::completion::message::ToolResultContent;
use rig_core::tool::ToolOutput;

use super::ContextShaper;

/// The recording cap never drops below this many tokens, however small the
/// budget: a model whose base alone fills the window is broken anyway, and a
/// result cut to nothing would hide why.
pub const RECORDING_CAP_FLOOR_TOKENS: usize = 512;

impl ContextShaper {
    /// Tokens a single tool result may occupy when it is recorded: two
    /// fifths of the history budget, so the newest two results fit together
    /// with a fifth to spare for the calls around them, with a floor of
    /// [`RECORDING_CAP_FLOOR_TOKENS`].
    pub fn recording_cap(&self) -> usize {
        (self.history_budget(0) * 2 / 5).max(RECORDING_CAP_FLOOR_TOKENS)
    }

    /// The presentation a tool result is recorded with: `output` itself when
    /// it fits the recording cap, else its text cut down to the cap — its
    /// head and its tail, with a header saying how much was kept and a
    /// marker where the middle was. Image blocks are never touched.
    pub fn record_tool_output(&self, output: &ToolOutput) -> Option<ToolOutput> {
        self.record_within(output, self.recording_cap())
    }

    /// Start a new tool round: the results recorded from here on share the
    /// room again (called before every model call).
    pub fn start_round(&self) {
        if let Ok(mut used) = self.inner.round_recorded.lock() {
            *used = 0;
        }
    }

    /// [`Self::record_tool_output`] for one result of the current round:
    /// capped at two fifths of the room the round's earlier results left in
    /// the history budget (never under [`RECORDING_CAP_FLOOR_TOKENS`]), so
    /// the round's results together fit it (AGE-978).
    pub fn record_round_output(&self, output: &ToolOutput) -> Option<ToolOutput> {
        let Ok(mut used) = self.inner.round_recorded.lock() else {
            return self.record_tool_output(output);
        };
        let room = self.history_budget(0).saturating_sub(*used);
        let cap = (room * 2 / 5).max(RECORDING_CAP_FLOOR_TOKENS);
        let recorded = self.record_within(output, cap);
        let sent = recorded.as_ref().unwrap_or(output);
        *used += sent
            .as_content()
            .iter()
            .map(|block| match block {
                ToolResultContent::Text(text) => self.inner.counter.count(&text.text),
                ToolResultContent::Json { value } => self.inner.counter.count(&value.to_string()),
                ToolResultContent::Image(_) => 0,
            })
            .sum::<usize>();
        recorded
    }

    /// `output` cut down to `cap` tokens, or `None` when it fits (or holds
    /// an image).
    pub(super) fn record_within(&self, output: &ToolOutput, cap: usize) -> Option<ToolOutput> {
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
        if tokens <= cap {
            return None;
        }
        // Keep the share of the text the cap allows, at this result's own
        // chars-per-token ratio, half from each end; the ratio of a part is
        // only close to the whole's, so recount and cut again while the kept
        // parts plus the header and marker are still over the cap.
        let cut_to = |keep: usize| {
            let (head, tail) = head_and_tail(&text, keep);
            let kept = head.len() + tail.len();
            format!(
                "[tool result truncated — kept {kept} of {} chars, from its start and its end; \
                 the rest did not fit the model's context window]\n{head}{}{tail}",
                text.len(),
                omitted_marker(text.len() - kept),
            )
        };
        let overhead = self.inner.counter.count(&cut_to(0));
        let budget = cap.saturating_sub(overhead);
        let mut keep = text.len() * budget / tokens;
        for _ in 0..4 {
            let cut = cut_to(keep);
            let kept_tokens = self.inner.counter.count(&cut).saturating_sub(overhead);
            if kept_tokens <= budget || keep == 0 {
                return Some(ToolOutput::text(cut));
            }
            keep = keep * budget / kept_tokens;
        }
        Some(ToolOutput::text(cut_to(0)))
    }
}

/// The first and the last of `text`, `keep` bytes in all (half from each
/// end), each cut on a char boundary; the whole text as the head when it is
/// no longer than `keep`.
pub(crate) fn head_and_tail(text: &str, keep: usize) -> (&str, &str) {
    if keep >= text.len() {
        return (text, "");
    }
    let head_end = text.floor_char_boundary(keep / 2);
    let tail_start = text.ceil_char_boundary(text.len() - (keep - keep / 2));
    (&text[..head_end], &text[tail_start.max(head_end)..])
}

/// What stands where a cut tool result's middle was (AGE-978): how much is
/// missing, and how a model reads it — the omitted part is still there to
/// be read in smaller pieces.
pub(crate) fn omitted_marker(omitted: usize) -> String {
    format!(
        "\n[… {omitted} chars omitted here. To read them, call the tool again for a \
         smaller part: read_file with start_line/end_line, or grep, head, tail or \
         sed -n in the shell …]\n"
    )
}
