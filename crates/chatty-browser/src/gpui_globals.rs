//! Implements `gpui::Global` for chatty-browser types.
//!
//! This module is only compiled when the `gpui-globals` feature is enabled.

use gpui::Global;

impl Global for crate::settings::BrowserSettingsModel {}
impl Global for crate::settings::BrowserCredentialsModel {}
