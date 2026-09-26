//! [`TerminalSource`] over the user's tmux server: `list-panes` to list,
//! `capture-pane` to read. A pane id (`%N`) is the terminal id.

use std::cmp::Reverse;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use async_trait::async_trait;

use super::{
    Region, TerminalAccess, TerminalBackend, TerminalInfo, TerminalKind, TerminalSource,
    TerminalText, trim_terminal_text,
};

/// A tmux call that takes longer than this is treated as a failure, so a
/// wedged server cannot hang an agent build or a tool call.
const TMUX_TIMEOUT: Duration = Duration::from_secs(5);

/// One `list-panes` row per pane. The title goes last so a tab inside it
/// cannot shift the other fields.
const LIST_FORMAT: &str = "#{pane_id}\t#{session_activity}\t#{window_active}\t#{pane_active}\t\
     #{pane_last}\t#{window_activity}\t#{session_name}:#{window_index}.#{pane_index}\t\
     #{pane_current_path}\t#{pane_current_command}\t#{host}\t#{pane_title}";

/// The line `display-message` prints ahead of the capture.
const READ_META_FORMAT: &str = "#{cursor_y}\t#{pane_width}\t#{pane_height}";

/// The user's tmux panes. By default this talks to whatever server a plain
/// `tmux` command would (the one in `$TMUX`, else the default socket).
#[derive(Clone, Debug, Default)]
pub struct TmuxSource {
    /// `tmux -L <name>`: an isolated server, for tests.
    socket_name: Option<String>,
}

impl TmuxSource {
    /// The user's own tmux server.
    pub fn new() -> Self {
        Self::default()
    }

    /// The server on socket `name` (`tmux -L name`) instead of the user's.
    pub fn with_socket_name(name: impl Into<String>) -> Self {
        Self {
            socket_name: Some(name.into()),
        }
    }

    /// The pane chatty itself runs in, when it runs inside tmux on the server
    /// this source reads. Left out of the list: reading its own chat back is
    /// never what the user means.
    fn own_pane(&self) -> Option<String> {
        if self.socket_name.is_some() {
            return None;
        }
        std::env::var("TMUX_PANE").ok().filter(|p| !p.is_empty())
    }

    async fn tmux(&self, args: &[&str]) -> anyhow::Result<std::process::Output> {
        let mut command = tokio::process::Command::new("tmux");
        if let Some(name) = &self.socket_name {
            command.arg("-L").arg(name);
        }
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        tokio::time::timeout(TMUX_TIMEOUT, command.output())
            .await
            .map_err(|_| anyhow!("tmux did not answer within {}s", TMUX_TIMEOUT.as_secs()))?
            .context("could not run tmux")
    }
}

#[async_trait]
impl TerminalSource for TmuxSource {
    async fn list(&self) -> Vec<TerminalInfo> {
        match self.tmux(&["list-panes", "-a", "-F", LIST_FORMAT]).await {
            Ok(output) if output.status.success() => parse_list_panes(
                &String::from_utf8_lossy(&output.stdout),
                self.own_pane().as_deref(),
            ),
            // No server running: tmux exits non-zero with "no server running"
            // or "error connecting to …".
            Ok(output) => {
                tracing::debug!(
                    stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                    "tmux list-panes failed; no tmux terminals"
                );
                Vec::new()
            }
            // tmux not installed, or it hung.
            Err(e) => {
                tracing::debug!(error = %e, "tmux unavailable; no tmux terminals");
                Vec::new()
            }
        }
    }

    async fn read(&self, id: &str, region: Region) -> anyhow::Result<TerminalText> {
        if !is_pane_id(id) {
            bail!("no terminal `{id}`: tmux terminal ids look like `%3`");
        }
        let start;
        let mut args = vec![
            "display-message",
            "-p",
            "-t",
            id,
            READ_META_FORMAT,
            ";",
            "capture-pane",
            "-p",
            "-J",
            "-t",
            id,
        ];
        if let Region::Scrollback { lines } = region
            && lines > 0
        {
            start = format!("-{lines}");
            args.extend(["-S", start.as_str()]);
        }
        let output = self.tmux(&args).await?;
        if !output.status.success() {
            bail!(
                "tmux could not read terminal `{id}`: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        parse_capture(&String::from_utf8_lossy(&output.stdout))
    }
}

/// `%` followed by digits: the only target form `read` accepts, so a model
/// cannot reach tmux's wider target syntax through the id.
fn is_pane_id(id: &str) -> bool {
    id.strip_prefix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// One parsed `list-panes` row, with what the ordering needs.
struct PaneRow {
    info: TerminalInfo,
    session_activity: u64,
    window_active: bool,
    pane_active: bool,
    pane_last: bool,
    window_activity: u64,
}

/// Parse `list-panes -a -F LIST_FORMAT` output into terminals, most recently
/// active first, without `own_pane`.
///
/// Most recently active: the session the user last typed in, then its current
/// window, then that window's active pane, then the pane that was active
/// before it (`pane_last`: where the user was before switching to chatty's
/// own pane), then the window with the latest output.
fn parse_list_panes(stdout: &str, own_pane: Option<&str>) -> Vec<TerminalInfo> {
    let mut rows: Vec<PaneRow> = stdout
        .lines()
        .filter_map(parse_pane_row)
        .filter(|row| Some(row.info.id.as_str()) != own_pane)
        .collect();
    rows.sort_by_key(|row| {
        Reverse((
            row.session_activity,
            row.window_active,
            row.pane_active,
            row.pane_last,
            row.window_activity,
        ))
    });
    rows.into_iter().map(|row| row.info).collect()
}

fn parse_pane_row(line: &str) -> Option<PaneRow> {
    let mut fields = line.splitn(11, '\t');
    let id = fields.next()?.to_string();
    if !is_pane_id(&id) {
        return None;
    }
    let number = |s: Option<&str>| s.and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0);
    let flag = |s: Option<&str>| s == Some("1");
    let session_activity = number(fields.next());
    let window_active = flag(fields.next());
    let pane_active = flag(fields.next());
    let pane_last = flag(fields.next());
    let window_activity = number(fields.next());
    let target = fields.next()?;
    let cwd = fields.next().filter(|p| !p.is_empty()).map(str::to_string);
    let command = fields.next().unwrap_or_default();
    let host = fields.next().unwrap_or_default();
    let pane_title = fields.next().unwrap_or_default();

    let mut title = format!("{target} {command}").trim_end().to_string();
    // tmux's default pane title is the host name, which says nothing.
    if !pane_title.is_empty() && pane_title != host {
        title.push_str(&format!(" — {pane_title}"));
    }
    Some(PaneRow {
        info: TerminalInfo {
            id,
            title,
            cwd,
            backend: TerminalBackend::Tmux,
            kind: TerminalKind::Human,
            // The `terminal_access` setting shares every pane, read-only.
            access: TerminalAccess::Read,
        },
        session_activity,
        window_active,
        pane_active,
        pane_last,
        window_activity,
    })
}

/// Parse `display-message -p READ_META_FORMAT ; capture-pane -p -J …`: the
/// metadata line, then the captured rows.
fn parse_capture(stdout: &str) -> anyhow::Result<TerminalText> {
    let (meta, capture) = stdout.split_once('\n').unwrap_or((stdout, ""));
    let mut fields = meta.split('\t').map(|v| v.trim().parse::<u16>());
    let mut next = |name: &str| {
        fields
            .next()
            .and_then(Result::ok)
            .ok_or_else(|| anyhow!("unexpected tmux output: no {name} in {meta:?}"))
    };
    let cursor_row = next("cursor row")?;
    let cols = next("width")?;
    let rows = next("height")?;
    Ok(TerminalText {
        text: trim_terminal_text(capture),
        cursor_row,
        cols,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned `list-panes` output: session `work` (typed in last) has chatty
    /// in the active pane %1 and a build in %0, the pane used before it;
    /// session `old` is idle.
    const LIST: &str = "\
%0\t1790000100\t1\t0\t1\t1790000150\twork:0.0\t/home/m/proj\tbash\thost\thost
%1\t1790000100\t1\t1\t0\t1790000150\twork:0.1\t/home/m/proj\tchatty-tui\thost\thost
%2\t1790000100\t0\t1\t0\t1790000190\twork:1.0\t/tmp\tvim\thost\tnotes.md
%5\t1790000000\t1\t1\t0\t1790000000\told:0.0\t\tbash\thost\t
";

    fn ids(terminals: &[TerminalInfo]) -> Vec<&str> {
        terminals.iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn list_orders_by_where_the_user_was_last() {
        let terminals = parse_list_panes(LIST, None);
        assert_eq!(ids(&terminals), ["%1", "%0", "%2", "%5"]);
    }

    #[test]
    fn list_leaves_out_chattys_own_pane_so_the_previous_pane_comes_first() {
        let terminals = parse_list_panes(LIST, Some("%1"));
        assert_eq!(ids(&terminals), ["%0", "%2", "%5"]);
    }

    #[test]
    fn list_rows_carry_title_cwd_backend_and_access() {
        let terminals = parse_list_panes(LIST, None);
        let build = terminals.iter().find(|t| t.id == "%0").unwrap();
        assert_eq!(build.title, "work:0.0 bash");
        assert_eq!(build.cwd.as_deref(), Some("/home/m/proj"));
        assert_eq!(build.backend, TerminalBackend::Tmux);
        assert_eq!(build.kind, TerminalKind::Human);
        assert_eq!(build.access, TerminalAccess::Read);

        let vim = terminals.iter().find(|t| t.id == "%2").unwrap();
        assert_eq!(vim.title, "work:1.0 vim — notes.md");

        let idle = terminals.iter().find(|t| t.id == "%5").unwrap();
        assert_eq!(idle.cwd, None);
    }

    #[test]
    fn list_keeps_tabs_inside_the_pane_title() {
        let terminals = parse_list_panes("%7\t1\t1\t1\t0\t1\ts:0.0\t/\tbash\th\ta\tb\n", None);
        assert_eq!(terminals[0].title, "s:0.0 bash — a\tb");
    }

    #[test]
    fn list_skips_lines_that_are_not_pane_rows() {
        assert!(parse_list_panes("", None).is_empty());
        assert!(parse_list_panes("no server running on /tmp/tmux-1000/default\n", None).is_empty());
    }

    #[test]
    fn capture_of_the_screen_is_trimmed_and_carries_cursor_and_size() {
        let out = "5\t80\t24\n$ cargo build   \nerror[E0425]: cannot find value `x`  \n$ \n\n\n\n";
        let text = parse_capture(out).unwrap();
        assert_eq!(text.cursor_row, 5);
        assert_eq!((text.cols, text.rows), (80, 24));
        assert_eq!(
            text.text,
            "$ cargo build\nerror[E0425]: cannot find value `x`\n$"
        );
    }

    #[test]
    fn capture_with_scrollback_keeps_history_rows_above_the_screen() {
        let out = "0\t40\t2\nold line 1\nold line 2\n\nscreen row\n   \n";
        let text = parse_capture(out).unwrap();
        assert_eq!(text.text, "old line 1\nold line 2\n\nscreen row");
        assert_eq!(text.rows, 2);
    }

    #[test]
    fn capture_of_an_empty_pane_is_empty_text() {
        let text = parse_capture("0\t80\t24\n\n\n").unwrap();
        assert_eq!(text.text, "");
    }

    #[test]
    fn capture_without_metadata_is_an_error() {
        assert!(parse_capture("").is_err());
        assert!(parse_capture("hello\nworld").is_err());
    }

    #[test]
    fn only_pane_ids_are_accepted_as_terminal_ids() {
        assert!(is_pane_id("%0"));
        assert!(is_pane_id("%123"));
        for bad in ["", "%", "0", "work:0.1", "%1;kill-server", "%-1"] {
            assert!(!is_pane_id(bad), "{bad:?} must be rejected");
        }
    }

    /// No tmux server on this socket: an empty list, not an error.
    #[tokio::test]
    async fn a_missing_server_lists_nothing() {
        let source =
            TmuxSource::with_socket_name(format!("chatty-no-such-server-{}", std::process::id()));
        assert!(source.list().await.is_empty());
    }
}
