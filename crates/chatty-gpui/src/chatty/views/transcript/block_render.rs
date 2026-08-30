use gpui::*;
use gpui_component::ActiveTheme;

use super::activity::ActivityGroup;
use super::approval::{ApprovalCard, ErrorBlock};
use super::artifact::ArtifactCard;
use super::diff::DiffHunkList;
use super::types::Block;

pub fn render_typed_block(block: &Block, _window: &mut Window, cx: &mut App) -> AnyElement {
    match block {
        Block::User { .. } | Block::Text { .. } | Block::Plan { .. } => div().into_any_element(),
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
            DiffHunkList::from_tool(id.0.to_string(), tool).into_any_element()
        }
        Block::Approval { approval, .. } => ApprovalCard::new(approval.clone()).into_any_element(),
        Block::Artifact { path, .. } => ArtifactCard::new(path.clone()).into_any_element(),
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
