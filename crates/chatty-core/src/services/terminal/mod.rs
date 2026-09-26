//! A read-only view of the human's terminals for the agent (AGE-577, stage 0
//! of the embedded-terminal epic).
//!
//! [`TerminalSource`] is the seam: `terminal_read` talks to it and nothing
//! else. [`TmuxSource`] is the first backend (the user's own tmux panes); the
//! embedded terminal becomes a second one behind the same trait. The enums
//! are `#[non_exhaustive]` so later regions (the last command) and backends
//! land without breaking a match elsewhere. Writing to a terminal (`run`) is
//! deliberately not part of the trait yet.

mod tmux;

use async_trait::async_trait;
use serde::Serialize;

pub use tmux::TmuxSource;

/// Which kind of terminal a [`TerminalInfo`] describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TerminalBackend {
    /// A pane of the user's tmux server.
    Tmux,
    /// A terminal embedded in chatty itself.
    Embedded,
}

/// One terminal a source can read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TerminalInfo {
    /// Stable id to pass back to [`TerminalSource::read`] (`%N` for tmux).
    pub id: String,
    /// Human-readable label: where it is and what runs in it.
    pub title: String,
    /// Working directory of the terminal's foreground process, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub backend: TerminalBackend,
    /// Whether the human has shared this terminal with the agent. A tmux pane
    /// is shared by the `terminal_access` setting as a whole.
    pub shared: bool,
}

/// Which part of a terminal to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Region {
    /// The rows currently visible.
    Screen,
    /// The visible rows plus up to `lines` rows of history above them.
    Scrollback { lines: usize },
}

/// What a read returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalText {
    /// Plain text (no escape sequences), trailing blanks trimmed from every
    /// line and trailing empty lines dropped.
    pub text: String,
    /// The cursor's row, 0-based from the top of the visible screen.
    pub cursor_row: u16,
    /// Terminal width in columns.
    pub cols: u16,
    /// Terminal height in rows (the visible screen).
    pub rows: u16,
}

/// A backend that can list and read terminals.
#[async_trait]
pub trait TerminalSource: Send + Sync {
    /// The terminals this source can read, most recently active first. A
    /// backend that is not there (tmux not installed, no server running) has
    /// no terminals: that is an empty list, not an error.
    async fn list(&self) -> Vec<TerminalInfo>;

    /// Read `region` of terminal `id`.
    async fn read(&self, id: &str, region: Region) -> anyhow::Result<TerminalText>;
}

/// Trim trailing blanks off every line and drop trailing empty lines.
pub(crate) fn trim_terminal_text(raw: &str) -> String {
    let mut lines: Vec<&str> = raw.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming_drops_trailing_blanks_and_empty_rows_only() {
        let raw = "  indented   \nerror: boom\t \n\n   \n\n";
        assert_eq!(trim_terminal_text(raw), "  indented\nerror: boom");
    }

    #[test]
    fn trimming_keeps_blank_rows_between_content() {
        assert_eq!(trim_terminal_text("a\n\n\nb\n"), "a\n\n\nb");
        assert_eq!(trim_terminal_text("\n\n  \n"), "");
    }

    #[test]
    fn terminal_info_serializes_backend_in_snake_case() {
        let info = TerminalInfo {
            id: "%3".into(),
            title: "t".into(),
            cwd: None,
            backend: TerminalBackend::Tmux,
            shared: true,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["backend"], "tmux");
        assert!(json.get("cwd").is_none());
    }
}
