//! Centralized color palette and style helpers for the TUI.
//!
//! All UI modules should read colors from here rather than hardcoding
//! `Color::*` values. This keeps the palette consistent and makes it trivial
//! to introduce alternate themes later.

use ratatui::style::{Color, Modifier, Style};

// ---------------------------------------------------------------------------
// Semantic palette
// ---------------------------------------------------------------------------

/// Primary accent — active commands, slash menu, assistant role.
pub const ACCENT: Color = Color::Cyan;
/// User role, positive / successful states.
pub const USER: Color = Color::Green;
/// Warnings, streaming cursor, approval prompts.
pub const WARNING: Color = Color::Yellow;
/// Tool calls, skills, git branch.
pub const TOOL: Color = Color::Magenta;
/// Errors, denied approvals, host-mode indicator.
pub const ERROR: Color = Color::Red;

/// Primary foreground text on the default terminal background.
pub const TEXT: Color = Color::White;
/// Subtle foreground text (version, secondary labels).
pub const TEXT_SUBTLE: Color = Color::Gray;
/// De-emphasized text (placeholders, muted descriptions, scrollbar track).
pub const MUTED: Color = Color::DarkGray;

/// Border color for chat / input blocks.
pub const BORDER: Color = Color::DarkGray;

/// Status-bar background.
pub const STATUS_BG: Color = Color::DarkGray;
/// Status-bar foreground.
pub const STATUS_FG: Color = Color::White;

// ---------------------------------------------------------------------------
// Style builders
// ---------------------------------------------------------------------------

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn accent() -> Style {
    Style::default().fg(ACCENT)
}

pub fn accent_bold() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn tool() -> Style {
    Style::default().fg(TOOL)
}

pub fn tool_bold() -> Style {
    Style::default().fg(TOOL).add_modifier(Modifier::BOLD)
}

pub fn warning() -> Style {
    Style::default().fg(WARNING)
}

pub fn error() -> Style {
    Style::default().fg(ERROR)
}

pub fn success() -> Style {
    Style::default().fg(USER)
}

pub fn success_bold() -> Style {
    Style::default().fg(USER).add_modifier(Modifier::BOLD)
}

pub fn border() -> Style {
    Style::default().fg(BORDER)
}

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn text_bold() -> Style {
    Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
}

pub fn text_subtle() -> Style {
    Style::default().fg(TEXT_SUBTLE)
}

/// Highlight style used for selected list rows in pickers / menus.
pub fn highlight_accent() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// Highlight style for the `@` mention picker.
pub fn highlight_user() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(USER)
        .add_modifier(Modifier::BOLD)
}

/// Status bar base style (dark background, white foreground).
pub fn status_bar() -> Style {
    Style::default().bg(STATUS_BG).fg(STATUS_FG)
}
