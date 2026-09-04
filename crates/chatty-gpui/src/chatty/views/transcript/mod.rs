//! Block-based transcript types and product widgets for the desktop chat view.
//!
//! Persistence stays as `MessageEntry` + `system_trace` JSON in chatty-core.
//! These types live only in chatty-gpui.

#![allow(dead_code, unused_imports)]

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
mod plan;
mod run_pin;
mod session_changes;
mod session_review_panel;
mod table;
mod ticker;
mod tool_row;
mod types;
mod verb;

mod artifact_header;

pub use action_bar::MessageActionBar;
pub use activity::{ActivityGroup, RunTally, classify_tool};
pub use adapter::{
    COLLAPSED_TURN_HEIGHT, adapt_message, adapt_message_with_trace, adapt_messages,
    adapt_messages_with_traces, attach_plan_block, block_visible_in_turn, format_worked_for,
    format_working_for, plan_turn_index, turn_has_work_fold,
};
pub use approval::{ApprovalCard, ChangeTray, ErrorBlock, PathChange};
pub use artifact_batch_card::ArtifactBatchCard;
pub use artifact_card::ArtifactCard;
pub use artifact_header::{
    ArtifactCopy, ArtifactCopyKind, ArtifactHeaderKind, ArtifactTabSpec, artifact_copy_control,
    artifact_header_tabs,
};
pub use artifact_kind::{
    ArtifactHeading, ArtifactVersion, INLINE_IMAGE_MAX_PX, ViewAnchor, artifact_display_title,
    artifact_file_name, artifact_format_token, artifact_language_for_path, artifact_meta_line,
    artifact_panel_title, artifact_version, attachment_image_path, chart_artifact_path, csv_shape,
    csv_stat_line, heading_index_for_line, inline_chat_attachments, is_chart_artifact_tool,
    is_code_artifact_path, is_image_artifact_tool, is_image_path, is_lane_a_browser_tool,
    is_markdown_artifact_path, is_pdf_artifact_tool, is_pdf_path, is_produced_file_tool,
    is_standalone_artifact_path, is_tabular_path, is_transcript_artifact_receipt,
    markdown_headings, produced_path_is_openable, read_artifact_source, resolve_artifact_path,
    source_line_from_anchor, tool_file_path,
};
pub use artifact_view::{
    ArtifactMode, ArtifactView, ArtifactViewEvent, new_artifact_view, presentation_on_open,
};
pub use block_render::render_typed_block;
pub use clarification::{ChosenOption, ClarificationCard, ClarificationSummary};
pub use diff::{DiffHunkList, DiffStatRow, word_spans};
pub use diff_parse::parse_unified_diff;
pub use plan::{PLAN_LIST_TOP_PADDING, PLAN_STRIP_HEIGHT, PlanBlock, PlanOverlay, PlanStrip};
pub use run_pin::{RunPin, RunPinKind};
pub use session_changes::{
    FileChange, SessionChangeBar, TurnFileOverview, collect_file_changes_from_tools,
    file_change_from_tool, file_changes_from_turn, merge_file_changes,
};
pub use table::{extract_table_preview, render_table_preview_card};
pub use ticker::HeadlineTicker;
pub use tool_row::ToolRow;
pub use types::{Block, BlockId, Turn, TurnRole};

pub const HEADLINE_TICK_MS: u64 = 550;
pub const HEADLINE_QUEUE_MAX: usize = 4;
pub const GLYPH_ROTATE_MS: u64 = 3100;
pub const GLYPH_OPACITY_MS: u64 = 1700;
