// chatty-gpui::chatty — desktop frontend root.
//
// Locally-defined (GPUI-specific):
//   controllers/   models/   services/   token_budget/   views/
//
// For UI-agnostic core types (auth, exporters, factories, repositories,
// tools), import directly from `chatty_core::…`. Earlier versions of this
// crate re-exported those modules here; the re-exports were removed because
// they hid which crate a definition lived in. If you grep for a definition
// and find nothing under this directory, it lives under
// `crates/chatty-core/src/`.

// Partially local (gpui-specific files + re-exports from core)
pub mod controllers;
pub mod models;
pub mod services;
pub mod token_budget;
pub mod views;

pub use controllers::{ChattyApp, GlobalChattyApp};
