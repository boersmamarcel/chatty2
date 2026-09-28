//! The turn's swarm as an indented tree — the `/swarm` command's output
//! (TB-5, AGE-667).
//!
//! Pure like [`crate::ui::plan`]: rows in, no ratatui types, so the layout is
//! testable without a frame. Each row is one node of
//! [`SwarmTrace::tree`](chatty_core::services::swarm_trace::SwarmTrace::tree):
//! agent · model · status · spend. `status` takes its tense from
//! `ui/verb.rs`'s running/done/failed convention (AGE-136), applied to a
//! node's [`NodeStatus`] instead of a tool call's state.

use chatty_core::services::swarm_trace::{AgentNode, NodeStatus, SwarmTrace, UsageLine};

/// Columns of indent per tree depth, matching [`crate::ui::plan`]'s step
/// indent style.
const INDENT_WIDTH: usize = 2;

/// `/swarm` when the turn made no delegation.
pub const NO_DELEGATION: &str = "No delegation yet this conversation.";

/// One row of the tree, already indented: `<indent><agent> · <model> ·
/// <status> · <spend>`.
pub fn swarm_rows(trace: &SwarmTrace) -> Vec<String> {
    let tree = trace.tree();
    tree.preorder()
        .into_iter()
        .map(|id| {
            let node = tree.get(id);
            let indent = " ".repeat(tree.depth(id) * INDENT_WIDTH);
            format!(
                "{indent}{} · {} · {} · {}",
                node.name,
                model_text(node),
                status_word(&node.status),
                spend_text(&node.usage),
            )
        })
        .collect()
}

/// `/swarm`'s full text: the indented tree, or [`NO_DELEGATION`] when the
/// turn made none.
pub fn render(trace: &SwarmTrace) -> String {
    if trace.tree().len() <= 1 {
        return NO_DELEGATION.to_string();
    }
    swarm_rows(trace).join("\n")
}

fn model_text(node: &AgentNode) -> String {
    node.model
        .as_ref()
        .map(|model| model.model_id.clone())
        .unwrap_or_else(|| "—".to_string())
}

/// The tense word for a node's status, in `ui/verb.rs`'s
/// running/done/failed style.
fn status_word(status: &NodeStatus) -> String {
    match status {
        NodeStatus::Running => "running".to_string(),
        NodeStatus::Completed => "done".to_string(),
        NodeStatus::Failed => "failed".to_string(),
        NodeStatus::Canceled => "canceled".to_string(),
        NodeStatus::Refused { reason } => format!("refused: {reason}"),
    }
}

/// The tokens a node spent on its own, summed over its usage lines: a fact,
/// not a price (the trace never converts tokens to cost, see its module
/// docs).
fn spend_text(usage: &[UsageLine]) -> String {
    let tokens: u64 = usage.iter().map(UsageLine::tokens).sum();
    format!("{tokens} tok")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_swarm_empty() {
        let trace = SwarmTrace::new();
        assert_eq!(render(&trace), NO_DELEGATION);
    }

    #[test]
    fn status_words_match_the_tense_convention() {
        assert_eq!(status_word(&NodeStatus::Running), "running");
        assert_eq!(status_word(&NodeStatus::Completed), "done");
        assert_eq!(status_word(&NodeStatus::Failed), "failed");
        assert_eq!(status_word(&NodeStatus::Canceled), "canceled");
        assert_eq!(
            status_word(&NodeStatus::Refused {
                reason: "depth".to_string()
            }),
            "refused: depth"
        );
    }
}
