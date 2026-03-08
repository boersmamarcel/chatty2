// Re-export everything from chatty-core token_budget (types + submodules)
pub use chatty_core::token_budget::*;

// Local gpui-specific module
pub mod manager;

pub use manager::{
    GlobalTokenBudget, check_pressure, compute_snapshot_background, extract_user_message_text,
    gather_snapshot_inputs,
};
