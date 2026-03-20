//! # chatty-browser
//!
//! Rust-native browser automation for chatty2, powered by [Verso](https://github.com/nicholasLiang/nickel-browser)
//! (a Servo-based browser engine).
//!
//! This crate provides:
//! - **`BrowserEngine`**: Manages the `versoview` sidecar process lifecycle
//! - **`BrowserSession`**: Per-tab browsing context with page state tracking
//! - **`DevToolsClient`**: Firefox Remote Debug Protocol client for automation
//! - **`BrowseTool`**: A rig-core `Tool` implementation for LLM agent integration
//! - **`PageSnapshot`**: Structured page representation optimized for LLM consumption
//!
//! ## Architecture
//!
//! ```text
//! Agent (chatty-core)
//!   └── BrowseTool
//!         └── BrowserEngine
//!               ├── versoview process (sidecar)
//!               └── DevToolsClient (TCP)
//!                     └── BrowserSession (per-tab)
//!                           └── PageSnapshot (DOM → structured text)
//! ```

pub mod devtools;
pub mod engine;
pub mod error;
pub mod page_repr;
pub mod session;
pub mod tools;

// Re-export primary types for convenience
pub use engine::{BrowserEngine, BrowserEngineConfig};
pub use error::BrowserError;
pub use page_repr::PageSnapshot;
pub use session::BrowserSession;
pub use tools::browse_tool::BrowseTool;
