// chatty-gpui::chatty — desktop frontend root.
//
// Most modules under this path live in `chatty-core` (the UI-agnostic crate)
// and are re-exported here for ergonomic `use crate::chatty::…` paths in
// view/controller code. If you grep for a definition under this directory
// and find nothing, look under `crates/chatty-core/src/` instead.
//
// Locally-defined (GPUI-specific):
//   controllers/   models/   services/   token_budget/   views/
//
// Re-exported from chatty-core (definitions live there):
//   auth, exporters, factories, repositories, tools

// Fully delegated to chatty-core
pub use chatty_core::auth;
pub use chatty_core::exporters;
pub use chatty_core::factories;
pub use chatty_core::repositories;
pub use chatty_core::tools;

// Partially local (gpui-specific files + re-exports from core)
pub mod controllers;
pub mod models;
pub mod services;
pub mod token_budget;
pub mod views;

pub use controllers::{ChattyApp, GlobalChattyApp};
