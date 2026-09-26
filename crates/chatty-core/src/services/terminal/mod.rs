//! A read-only view of the human's terminals for the agent (AGE-577, stage 0
//! of the embedded-terminal epic).
//!
//! [`TerminalSource`] is the seam: `terminal_read` talks to it and nothing
//! else. [`TmuxSource`] is the first backend (the user's own tmux panes); the
//! desktop's embedded terminals are a second one (AGE-583, in chatty-gpui),
//! and [`TerminalSources`] lists and reads several as one. The enums are
//! `#[non_exhaustive]` so later regions (the last command) and backends land
//! without breaking a match elsewhere. Writing to a terminal (`run`) is
//! deliberately not part of the trait yet.

mod tmux;

use std::sync::Arc;

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

/// Whose terminal it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TerminalKind {
    /// The human's own terminal: a tmux pane or a tab they opened.
    Human,
    /// The agent's own shell (the dock's Agent tab), always readable by it.
    Agent,
}

/// How much of a terminal the human lets the agent have.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAccess {
    /// Not shared: listed, never read.
    #[default]
    None,
    /// The agent may read the screen and scrollback.
    Read,
    /// The agent may also run commands there, each behind an approval.
    ReadRun,
}

impl TerminalAccess {
    /// Whether the agent may read the terminal at this level.
    pub fn can_read(self) -> bool {
        !matches!(self, Self::None)
    }
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
    pub kind: TerminalKind,
    /// What the human has shared. A tmux pane is shared (`read`) by the
    /// `terminal_access` setting as a whole; an embedded tab one by one.
    pub access: TerminalAccess,
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

/// What `read` fails with when the terminal is at a password prompt (echo
/// off), instead of returning its screen. Every embedded terminal checks,
/// the agent's own included; tmux cannot tell, and Windows has no termios.
pub const HIDDEN_INPUT_MESSAGE: &str = "terminal is at a hidden-input prompt (a password or \
     passphrase is being typed), so its screen was not read. Read it again once the prompt \
     is answered.";

/// Several sources listed and read as one: `list` is each source's list in
/// order (put the one whose order should win first), `read` goes to the
/// source that lists the id.
pub struct TerminalSources(Vec<Arc<dyn TerminalSource>>);

impl TerminalSources {
    pub fn new(sources: Vec<Arc<dyn TerminalSource>>) -> Self {
        Self(sources)
    }
}

#[async_trait]
impl TerminalSource for TerminalSources {
    async fn list(&self) -> Vec<TerminalInfo> {
        let mut all = Vec::new();
        for source in &self.0 {
            all.extend(source.list().await);
        }
        all
    }

    async fn read(&self, id: &str, region: Region) -> anyhow::Result<TerminalText> {
        for source in &self.0 {
            if source.list().await.iter().any(|t| t.id == id) {
                return source.read(id, region).await;
            }
        }
        anyhow::bail!("no terminal `{id}`; `list: true` lists the terminals")
    }
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
            kind: TerminalKind::Human,
            access: TerminalAccess::ReadRun,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["backend"], "tmux");
        assert_eq!(json["kind"], "human");
        assert_eq!(json["access"], "read_run");
        assert!(json.get("cwd").is_none());
    }
}
