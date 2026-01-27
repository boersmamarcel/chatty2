//! Auto-updater module for Chatty
//!
//! This module provides automatic update functionality including:
//! - Background polling for new releases
//! - Version comparison using semver
//! - Binary downloading with progress tracking
//! - OS-specific installation strategies (macOS, Linux, Windows)
//!
//! Inspired by Zed editor's auto-update architecture.

mod installer;
mod model;
mod release_source;

pub use installer::*;
pub use model::*;
pub use release_source::*;
