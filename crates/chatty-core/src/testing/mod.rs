//! Test kits other crates build their tests on (AGE-632).
//!
//! [`fake_model`] is a localhost model server that answers from a script and
//! records every request, so a test that spans processes — a leader, real
//! `chatty-tui` workers, a broker — runs with no network and no real model.

pub mod append_only;
pub mod fake_model;

/// `name` as a rig tool name, for tests that build tool calls by hand
/// (rig 0.46 types tool names; AGE-1151).
pub fn tool_name(name: impl Into<String>) -> rig_core::message::ToolName {
    rig_core::message::ToolName::new(name).expect("a test tool name is not empty")
}
