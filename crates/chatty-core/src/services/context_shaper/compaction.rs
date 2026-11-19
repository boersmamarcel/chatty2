//! LLM compaction: an old part of the history replaced by one summary
//! message, remembered so every later request carries the same one.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rig_core::completion::Message;

use crate::models::token_usage::ApiCallUsage;
use crate::services::context_compaction::{
    Summarizer, SummaryInputs, build_summary, task_max_chars,
};

use super::ContextShaper;
use super::stages::round_trip_safe_tail_start;

/// A history over this share of the budget is compacted. Early enough that
/// the summary call and the next few turns still fit, late enough that a run
/// which fits is never touched.
pub const COMPACTION_TRIGGER_FRACTION: f64 = 0.85;

/// Share of the budget the most recent messages may keep verbatim when the
/// history is compacted.
pub const COMPACTION_KEEP_RECENT_FRACTION: f64 = 0.40;

/// A compaction must take at least this share of the budget out of the
/// verbatim part, or it is not worth a model call: without the floor, a
/// history whose tail alone sits near the trigger would compact on every
/// request. What it cannot free is left to the stages.
pub(super) const COMPACTION_MIN_FREED_FRACTION: f64 = 0.25;

/// A compaction in force: `history[..covered]` goes out as `summary`.
#[derive(Clone)]
pub(super) struct Compaction {
    covered: usize,
    /// `history[covered - 1]` when the compaction was made: a history that
    /// no longer carries it at that index is a different one.
    boundary: Message,
    summary: Message,
    prose: String,
}

impl Compaction {
    pub(super) fn applies_to(&self, history: &[Message]) -> bool {
        history.len() >= self.covered && history[self.covered - 1] == self.boundary
    }

    pub(super) fn apply(&self, history: &[Message]) -> Vec<Message> {
        std::iter::once(self.summary.clone())
            .chain(history[self.covered..].iter().cloned())
            .collect()
    }
}

impl ContextShaper {
    /// Turn compaction on for this agent: `summarizer` writes the prose (the
    /// agent's tool-free utility agent; `None` keeps the deterministic
    /// summary) and `workspace` is where `git status` is taken. Off until
    /// called; the agent factory calls it for unattended runs only, so an
    /// interactive chat never waits on a summary call mid-turn.
    pub fn enable_compaction(
        &self,
        summarizer: Option<Arc<dyn Summarizer>>,
        workspace: Option<PathBuf>,
    ) {
        if let Some(summarizer) = summarizer {
            let _ = self.inner.summarizer.set(summarizer);
        }
        if let Some(dir) = workspace {
            let _ = self.inner.workspace.set(dir);
        }
        self.inner.compaction_enabled.store(true, Ordering::Relaxed);
    }

    /// The usage of every compaction summary call made since the last time
    /// this was asked (AGE-683). The stream folds it into the turn the call
    /// ran in, so the call is counted and priced like any other.
    pub fn take_compaction_usage(&self) -> Vec<ApiCallUsage> {
        self.inner
            .compaction_usage
            .lock()
            .map(|mut usage| std::mem::take(&mut *usage))
            .unwrap_or_default()
    }

    /// The history with the compaction in force applied, compacting (again)
    /// first when that is over the trigger. `None` when no compaction has
    /// happened and none is due: the history goes on as recorded.
    pub(super) async fn compacted(
        &self,
        history: &[Message],
        prompt_tokens: usize,
        turn: usize,
    ) -> Option<Vec<Message>> {
        if !self.inner.compaction_enabled.load(Ordering::Relaxed) {
            return None;
        }
        let settings = &self.inner.settings;
        let fraction = settings.compaction_trigger_fraction;
        let counter = &self.inner.counter;
        let budget = self.history_budget(prompt_tokens);
        let trigger = (budget as f64 * fraction) as usize;

        let previous = self
            .inner
            .compaction
            .lock()
            .ok()
            .and_then(|state| state.clone())
            .filter(|c| c.applies_to(history));
        let current = match &previous {
            Some(c) => c.apply(history),
            None => history.to_vec(),
        };
        let current_tokens: usize = current.iter().map(|m| counter.count_message(m)).sum();
        if current_tokens <= trigger {
            return previous.map(|_| current);
        }

        // Keep the newest messages up to their share of the budget, at least
        // the last one, and never half a round-trip (AGE-512).
        let offset = previous.as_ref().map_or(0, |c| c.covered);
        let keep_budget = (budget as f64 * settings.compaction_keep_recent_fraction) as usize;
        let mut start = history.len();
        let mut kept = 0usize;
        while start > offset + 1 {
            let tokens = counter.count_message(&history[start - 1]);
            if start < history.len() && kept + tokens > keep_budget {
                break;
            }
            kept += tokens;
            start -= 1;
        }
        let start = offset + round_trip_safe_tail_start(&history[offset..], 0, start - offset);
        let freed: usize = history[offset..start]
            .iter()
            .map(|m| counter.count_message(m))
            .sum();
        if start <= offset || (freed as f64) < budget as f64 * COMPACTION_MIN_FREED_FRACTION {
            return previous.map(|_| current);
        }

        let transcript_max_chars = budget.saturating_mul(2);
        let summary = build_summary(SummaryInputs {
            history,
            covered: start,
            previous_prose: previous.as_ref().map(|c| c.prose.as_str()),
            workspace: self.inner.workspace.get(),
            summarizer: self.inner.summarizer.get().map(|s| s.as_ref()),
            transcript_max_chars,
            task_max_chars: task_max_chars(budget),
        })
        .await;
        if let Some(usage) = summary.usage
            && let Ok(mut pending) = self.inner.compaction_usage.lock()
        {
            pending.push(usage);
        }
        let compaction = Compaction {
            covered: start,
            boundary: history[start - 1].clone(),
            summary: summary.message,
            prose: summary.prose,
        };
        let compacted = compaction.apply(history);
        let tokens_after: usize = compacted.iter().map(|m| counter.count_message(m)).sum();
        tracing::info!(
            turn,
            replaced = start,
            kept = history.len() - start,
            tokens_before = current_tokens,
            tokens_after,
            budget,
            "context shaper: history compacted"
        );
        if let Ok(mut state) = self.inner.compaction.lock() {
            *state = Some(compaction);
        }
        Some(compacted)
    }
}
