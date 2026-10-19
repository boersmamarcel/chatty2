//! Block-based transcript types and product widgets for the desktop chat view.
//!
//! Persistence stays as `MessageEntry` + `system_trace` JSON in chatty-core.
//! These types live only in chatty-gpui.

use std::path::PathBuf;
use std::rc::Rc;

use gpui::App;

/// Payload when opening a file in the artifact workbench.
#[derive(Clone, Debug)]
pub struct ArtifactOpen {
    pub path: PathBuf,
    pub source: String,
    pub old: Option<String>,
}

/// Open a produced file in the right-hand artifact workbench.
pub type OpenArtifact = Rc<dyn Fn(ArtifactOpen, &mut App)>;

/// Open a structured query snapshot in the artifact table view.
#[derive(Clone, Debug)]
pub struct TableOpen {
    pub preview: chatty_core::tools::data_query_tool::TablePreview,
}

pub type OpenTable = Rc<dyn Fn(TableOpen, &mut App)>;

mod action_bar;
mod activity;
mod adapter;
mod approval;
mod artifact_batch_card;
mod artifact_card;
mod artifact_kind;
mod artifact_view;
mod block_render;
mod clarification;
mod diff;
mod diff_parse;
/// `pub(crate)` (AGE-480): the sidebar's Files mode
/// (`crate::chatty::views::sidebar_file_tree`) owns the tree now and needs
/// this model from outside the `transcript` module tree.
pub(crate) mod file_tree;
mod markdown_mermaid;
mod plan;
mod run_pin;
mod session_changes;
mod session_review_panel;
mod swarm_tree;
mod table;
mod ticker;
mod tool_row;
mod types;
mod verb;

mod artifact_header;

pub use action_bar::MessageActionBar;
pub use activity::{
    ActivityGroup, LIVE_FADE_MS, RunTally, classify_tool, live_headline, phase_label,
};
#[cfg(test)]
pub use adapter::adapt_messages_with_traces;
pub use adapter::{
    COLLAPSED_TURN_HEIGHT, adapt_message_with_trace, attach_plan_block, block_visible_in_turn,
    format_worked_for, format_working_for, plan_turn_index, turn_has_work_fold,
};
pub use approval::ApprovalCard;
pub use artifact_card::reveal_path_in_os;
pub use artifact_kind::{
    INLINE_IMAGE_MAX_PX, attachment_image_path, inline_chat_attachments, is_lane_a_browser_tool,
    is_pdf_artifact_tool, is_pdf_path, produced_path_is_openable, read_artifact_source,
    resolve_artifact_path, tool_file_path,
};
pub use artifact_view::{ArtifactMode, ArtifactView, ArtifactViewEvent, new_artifact_view};
pub use block_render::{SwarmActions, render_typed_block};
pub use clarification::{ChosenOption, ClarificationCard, ClarificationSummary};
pub use plan::{PLAN_LIST_TOP_PADDING, PlanStrip};
pub use run_pin::{RunPin, RunPinKind};
pub use session_changes::{
    FileChange, SessionChangeBar, TurnFileOverview, file_change_from_tool, file_changes_from_turn,
    merge_file_changes,
};
#[cfg(test)]
pub use swarm_tree::SwarmNodeView;
pub use swarm_tree::{AgentTranscript, SwarmFold, SwarmTree, adapt_swarm_tree, delegation_callees};
pub use table::{extract_table_preview, render_table_preview_card};
pub use ticker::HeadlineTicker;
pub use tool_row::ToolRow;
#[cfg(test)]
pub use types::BlockId;
pub use types::{Block, Turn, TurnRole};

pub const HEADLINE_TICK_MS: u64 = 550;
pub const HEADLINE_QUEUE_MAX: usize = 4;
pub const GLYPH_ROTATE_MS: u64 = 3100;
pub const GLYPH_OPACITY_MS: u64 = 1700;
