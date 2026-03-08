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
