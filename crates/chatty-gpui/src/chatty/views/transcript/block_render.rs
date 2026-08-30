use chatty_core::services::AgentTaskSnapshot;
use gpui::*;
use gpui_component::ActiveTheme;

use super::OpenArtifact;
use super::activity::ActivityGroup;
use super::approval::{ApprovalCard, ErrorBlock};
use super::artifact::ArtifactCard;
use super::diff::DiffHunkList;
use super::plan::PlanBlock;
use super::types::Block;

pub fn render_typed_block(
    block: &Block,
    on_open: Option<OpenArtifact>,
    plan: Option<&AgentTaskSnapshot>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match block {
        Block::User { .. } | Block::Text { .. } => div().into_any_element(),
        Block::Plan { .. } => match plan {
            Some(snapshot) if snapshot.write_todos_called && !snapshot.todos.is_empty() => {
                PlanBlock::new(snapshot.clone()).into_any_element()
            }
            _ => div().into_any_element(),
        },
        Block::Thinking { id, block } => div()
            .id(id.element_id())
            .px_3()
            .py_1()
            .text_xs()
            .italic()
            .text_color(cx.theme().muted_foreground)
            .child(if block.summary.is_empty() {
                format!("Thought {}", format_secs(block.duration))
            } else {
                block.summary.clone()
            })
            .into_any_element(),
        Block::Activity { tools, .. } => ActivityGroup::new(tools.clone()).into_any_element(),
        Block::Diff { id, tool } => {
            let mut hunk = DiffHunkList::from_tool(id.0.to_string(), tool);
            if let Some(on_open) = on_open.clone() {
                hunk = hunk.on_open(move |path, source, cx| on_open(path, source, cx));
            }
            hunk.into_any_element()
        }
        Block::Approval { approval, .. } => ApprovalCard::new(approval.clone()).into_any_element(),
        Block::Artifact { path, .. } => {
            let mut card = ArtifactCard::new(path.clone());
            if let Some(on_open) = on_open.clone() {
                card = card.on_open(move |path, source, cx| on_open(path, source, cx));
            }
            card.into_any_element()
        }
        Block::Error {
            id,
            message,
            detail,
        } => ErrorBlock::new(id.0.to_string(), message.clone(), detail.clone()).into_any_element(),
    }
}

fn format_secs(duration: Option<std::time::Duration>) -> String {
    match duration.map(|d| d.as_secs()).unwrap_or(0) {
        0 => "a moment".to_string(),
        n => format!("{n}s"),
    }
}
