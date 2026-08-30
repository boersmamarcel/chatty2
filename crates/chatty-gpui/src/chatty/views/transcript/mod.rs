//! Block-based transcript types and product widgets for the desktop chat view.
//!
//! Persistence stays as `MessageEntry` + `system_trace` JSON in chatty-core.
//! These types live only in chatty-gpui.

#![allow(dead_code, unused_imports)]

mod action_bar;
mod activity;
mod adapter;
mod approval;
mod artifact;
mod diff;
mod plan;
mod run_pin;
mod ticker;
mod tool_row;
mod types;
mod verb;

pub use action_bar::MessageActionBar;
pub use activity::{ActivityGroup, RunTally, classify_tool};
pub use adapter::{
    COLLAPSED_TURN_HEIGHT, adapt_message, adapt_messages, estimate_turn_height, format_worked_for,
};
pub use approval::{ApprovalCard, ChangeTray, ErrorBlock, PathChange};
pub use artifact::{ArtifactCard, ArtifactMode, ArtifactView};
pub use diff::{DiffHunkList, DiffStatRow, word_spans};
pub use plan::{PlanBlock, PlanOverlay, PlanStrip};
pub use run_pin::{RunPin, RunPinKind};
pub use ticker::HeadlineTicker;
pub use tool_row::ToolRow;
pub use types::{Block, BlockId, Turn, TurnRole};

pub const HEADLINE_TICK_MS: u64 = 550;
pub const HEADLINE_QUEUE_MAX: usize = 4;
pub const GLYPH_ROTATE_MS: u64 = 3100;
pub const GLYPH_OPACITY_MS: u64 = 1700;
