//! Plain-text snapshots of the terminal grid.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};

use crate::commands::LastCommand;

/// Which part of the terminal to read.
///
/// Same shape as chatty-core's `Region` (AGE-577) so chatty-core can later
/// re-export this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Region {
    /// The rows currently visible in the viewport.
    Screen,
    /// The last `lines` rows of scrollback plus screen, ending at the last
    /// row with content. Rows, not logical lines: a soft-wrapped line
    /// counts once per row it covers.
    Scrollback { lines: usize },
    /// The last command run at an integrated shell's prompt (OSC 133): the
    /// text is its output, capped at
    /// [`LAST_COMMAND_OUTPUT_BYTES`](crate::LAST_COMMAND_OUTPUT_BYTES) (the
    /// end is kept), and [`TerminalText::last_command`] says what ran and
    /// how it exited. Without integration the text says so; it is not an
    /// error.
    LastCommand,
}

/// Plain text read from the terminal.
///
/// Wide characters appear once, soft-wrapped rows are joined into one line,
/// trailing blanks on each line and trailing blank lines are trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalText {
    pub text: String,
    /// Cursor row, counted from the top of the visible screen (0-based).
    pub cursor_row: u16,
    pub cols: u16,
    pub rows: u16,
    /// Set for [`Region::LastCommand`] only.
    pub last_command: Option<LastCommand>,
}

/// `text` with `term`'s cursor and size.
pub(crate) fn frame<T>(
    term: &Term<T>,
    text: String,
    last_command: Option<LastCommand>,
) -> TerminalText {
    TerminalText {
        text,
        cursor_row: term.grid().cursor.point.line.0.max(0) as u16,
        cols: term.columns() as u16,
        rows: term.screen_lines() as u16,
        last_command,
    }
}

/// A grid region; [`Region::LastCommand`] is answered by the command
/// tracker instead.
pub(crate) fn snapshot<T>(term: &Term<T>, region: Region) -> TerminalText {
    let grid = term.grid();

    let (top, bottom) = match region {
        Region::Screen => {
            let top = Line(-(grid.display_offset() as i32));
            (top, top + (term.screen_lines() as i32 - 1))
        }
        Region::Scrollback { lines } => {
            let mut bottom = term.bottommost_line();
            while bottom > term.topmost_line() && grid[bottom].is_clear() {
                bottom -= 1;
            }
            let wanted = lines.max(1) as i32;
            let top = Line((bottom.0 - wanted + 1).max(term.topmost_line().0));
            (top, bottom)
        }
        Region::LastCommand => return frame(term, String::new(), None),
    };

    let mut text = term.bounds_to_string(
        Point::new(top, Column(0)),
        Point::new(bottom, term.last_column()),
    );
    text.truncate(text.trim_end().len());
    frame(term, text, None)
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::mock_term;

    use super::*;

    #[test]
    fn soft_wrapped_rows_join_and_blank_rows_trim() {
        // `\n` soft-wraps (WRAPLINE), `\r\n` is a hard break.
        let term = mock_term("hello\n:)\r\ntest\r\n\r\n");
        let snap = snapshot(&term, Region::Screen);
        assert_eq!(snap.text, "hello:)\ntest");
        assert_eq!((snap.cols, snap.rows), (5, 5));
    }

    #[test]
    fn scrollback_of_an_empty_terminal_is_empty() {
        let size = crate::GridSize { cols: 10, rows: 5 };
        let term = Term::new(Default::default(), &size, VoidListener);
        assert_eq!(snapshot(&term, Region::Scrollback { lines: 10 }).text, "");
    }
}
