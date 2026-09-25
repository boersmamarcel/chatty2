use std::rc::Rc;

use chatty_core::services::AgentTaskSnapshot;
use gpui::*;
use gpui_component::ActiveTheme;

use super::OpenArtifact;
use super::OpenTable;
use super::activity::ActivityGroup;
use super::approval::ApprovalCard;
use super::artifact_batch_card::ArtifactBatchCard;
use super::artifact_card::ArtifactCard;
use super::clarification::ClarificationSummary;
use super::diff::DiffHunkList;
use super::plan::PlanBlock;
use super::table::render_table_preview_card;
use super::types::Block;

pub type ActivityToggle = Rc<dyn Fn(u64, &mut App)>;

#[allow(clippy::too_many_arguments)]
pub fn render_typed_block(
    block: &Block,
    message_index: usize,
    on_open: Option<OpenArtifact>,
    on_open_table: Option<OpenTable>,
    plan: Option<&AgentTaskSnapshot>,
    activity_open: Option<bool>,
    activity_live: bool,
    on_activity_toggle: Option<ActivityToggle>,
    open_artifact: Option<&std::path::Path>,
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
        Block::Activity { id, tools } => {
            // Collapsed until the user opens it, failures included.
            let open = activity_open.unwrap_or(false);
            let mut group = ActivityGroup::new(tools.clone())
                .open(open)
                .live(activity_live);
            if let Some(toggle) = on_activity_toggle {
                let block_id = id.0;
                group = group.on_toggle(move |cx| toggle(block_id, cx));
            }
            group.into_any_element()
        }
        Block::Diff { id, tool } => {
            let mut hunk = DiffHunkList::from_tool(id.0.to_string(), tool);
            if let Some(on_open) = on_open.clone() {
                hunk = hunk.on_open(move |open, cx| on_open(open, cx));
            }
            hunk.into_any_element()
        }
        Block::Approval { approval, .. } => ApprovalCard::new(approval.clone()).into_any_element(),
        Block::Clarification { clarification, .. } => {
            ClarificationSummary::new(clarification.clone()).into_any_element()
        }
        Block::Artifact {
            path, old_content, ..
        } => {
            let mut card = ArtifactCard::new(path.clone())
                .old_content(old_content.clone())
                .open(open_artifact.is_some_and(|open| open == path.as_path()));
            if let Some(on_open) = on_open.clone() {
                card = card.on_open(move |open, cx| on_open(open, cx));
            }
            card.into_any_element()
        }
        Block::ArtifactBatch { files, .. } => {
            let open_path = open_artifact.map(|p| p.to_path_buf());
            let mut card = ArtifactBatchCard::new(files.clone()).open_path(open_path);
            if let Some(on_open) = on_open.clone() {
                card = card.on_open(move |open, cx| on_open(open, cx));
            }
            card.into_any_element()
        }
        Block::TablePreview { id, preview } => render_table_preview_card(
            preview.clone(),
            message_index,
            id.0 as usize,
            on_open_table.clone(),
            cx,
        )
        .into_any_element(),
    }
}

fn format_secs(duration: Option<std::time::Duration>) -> String {
    match duration.map(|d| d.as_secs()).unwrap_or(0) {
        0 => "a moment".to_string(),
        n => format!("{n}s"),
    }
}
