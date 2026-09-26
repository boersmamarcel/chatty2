//! Shell-integration marks and clean text, read from the raw PTY byte stream.
//!
//! Feed a [`MarkScanner`] the bytes a byte tap sees (see [`crate::tap`]). It
//! reports the semantic-prompt marks a shell prints (OSC 133 `A`/`B`/`C`/`D`)
//! and chatty's private OSC [`CHATTY_OSC`], and turns the text between them
//! into [`CleanText`]: escape sequences dropped, `\r\n` as `\n`, and lines a
//! program redrew in place (progress bars: `\r`, erase-line, cursor-up)
//! reduced to how they ended. The text is read from the stream, not the grid,
//! so it does not wrap at the terminal width.

use std::collections::VecDeque;

use alacritty_terminal::vte::{Params, Parser, Perform};

/// Chatty's private OSC number (`ESC ] 6973 ; … BEL`). Unassigned by any
/// terminal; its fields are chatty's own (see [`Mark::Private`]).
pub const CHATTY_OSC: &str = "6973";

/// A mark found in the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// OSC 133;A — a prompt starts.
    PromptStart,
    /// OSC 133;B — the prompt ended; command input starts.
    CommandStart,
    /// OSC 133;C — the command was accepted; its output starts.
    OutputStart,
    /// OSC 133;D[;exit] — the command finished.
    CommandEnd { exit_code: Option<i32> },
    /// OSC 6973;… — chatty's own mark, with its `;`-separated fields.
    Private(Vec<String>),
}

/// Scans a PTY byte stream for [`Mark`]s and keeps the [`CleanText`] printed
/// since the last one.
pub struct MarkScanner {
    parser: Parser,
    text: CleanText,
}

impl Default for MarkScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkScanner {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            text: CleanText::default(),
        }
    }

    /// Scan `bytes`. `on_mark` runs at each mark, in stream order, with the
    /// text printed since the previous one; it decides whether text is kept
    /// from here on ([`CleanText::set_enabled`]) and takes what it wants.
    /// Sequences split across calls are carried over.
    pub fn feed(&mut self, mut bytes: &[u8], mut on_mark: impl FnMut(Mark, &mut CleanText)) {
        while !bytes.is_empty() {
            let (used, mark) = self.next_mark(bytes);
            bytes = &bytes[used..];
            if let Some(mark) = mark {
                on_mark(mark, &mut self.text);
            }
        }
    }

    /// Scan `bytes` up to the end of the next mark. Returns how many bytes
    /// were scanned (all of them when no mark ends in `bytes`) and the mark,
    /// so a caller can act at the exact point in the stream where it ended.
    pub fn next_mark(&mut self, bytes: &[u8]) -> (usize, Option<Mark>) {
        let mut performer = Performer {
            text: &mut self.text,
            mark: None,
        };
        let used = self.parser.advance_until_terminated(&mut performer, bytes);
        (used, performer.mark)
    }

    /// The text printed since the last mark, if kept.
    pub fn text(&self) -> &CleanText {
        &self.text
    }

    pub fn text_mut(&mut self) -> &mut CleanText {
        &mut self.text
    }
}

struct Performer<'a> {
    text: &'a mut CleanText,
    /// The mark that ended the scan.
    mark: Option<Mark>,
}

impl Perform for Performer<'_> {
    fn print(&mut self, c: char) {
        self.text.print(c);
    }

    fn execute(&mut self, byte: u8) {
        self.text.execute(byte);
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        if intermediates.is_empty() {
            self.text.csi(params, action);
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if intermediates.is_empty() {
            match byte {
                b'7' => self.text.save_cursor(),
                b'8' => self.text.restore_cursor(),
                _ => {}
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.mark = parse_mark(params);
    }

    fn terminated(&self) -> bool {
        self.mark.is_some()
    }
}

fn parse_mark(params: &[&[u8]]) -> Option<Mark> {
    let field = |i: usize| {
        params
            .get(i)
            .map(|p| String::from_utf8_lossy(p).into_owned())
    };
    match *params.first()? {
        b"133" => Some(match *params.get(1)? {
            b"A" => Mark::PromptStart,
            b"B" => Mark::CommandStart,
            b"C" => Mark::OutputStart,
            b"D" => Mark::CommandEnd {
                exit_code: field(2).and_then(|code| code.parse().ok()),
            },
            _ => return None,
        }),
        p if p == CHATTY_OSC.as_bytes() => {
            Some(Mark::Private((1..params.len()).filter_map(field).collect()))
        }
        _ => None,
    }
}

/// Lines further above the cursor than this are final: a program can only
/// move the cursor up within the screen, and no screen is this tall.
const LIVE_LINES: usize = 512;

/// Plain text rebuilt from a stream of terminal output, one line per `\n`,
/// with no width limit.
///
/// Printing overwrites at the cursor, so a line redrawn with `\r`, erase
/// line or cursor movement ends as it looked last. Colours and other
/// escape sequences are dropped. Absolute cursor positioning cannot be
/// placed in unwrapped text: between a cursor save and restore (a status
/// line drawn elsewhere, as apt does) what is printed is dropped; outside
/// one (`clear`) the text continues on a new line.
#[derive(Debug, Default)]
pub struct CleanText {
    enabled: bool,
    /// Lines that can no longer change, each ending in `\n`.
    done: String,
    /// The lines the cursor can still reach.
    live: VecDeque<Vec<char>>,
    row: usize,
    col: usize,
    saved: Option<(usize, usize)>,
    /// Printing somewhere that has no place in the text (see above).
    detached: bool,
}

impl CleanText {
    /// Keep (or stop keeping) text from here on. Turning it off drops what
    /// was kept.
    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.take();
        }
        self.enabled = enabled;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Whether nothing but whitespace was kept.
    pub fn is_blank(&self) -> bool {
        self.done.trim().is_empty()
            && self
                .live
                .iter()
                .all(|l| l.iter().all(|c| c.is_whitespace()))
    }

    /// The text kept so far.
    pub fn text(&self) -> String {
        let mut text = self.done.clone();
        for (i, line) in self.live.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            text.extend(line.iter());
        }
        text
    }

    /// Return the text kept so far and start over (the cursor with it).
    pub fn take(&mut self) -> String {
        let text = self.text();
        let enabled = self.enabled;
        *self = Self::default();
        self.enabled = enabled;
        text
    }

    /// Put the cursor at column `col` of its line, as when the text is kept
    /// from partway along a line (a command line after its prompt), so that
    /// later cursor movement by column lands where the terminal's does.
    pub fn move_to_column(&mut self, col: usize) {
        self.col = col;
    }

    fn line(&mut self) -> &mut Vec<char> {
        if self.live.is_empty() {
            self.live.push_back(Vec::new());
        }
        while self.row >= self.live.len() {
            self.live.push_back(Vec::new());
        }
        &mut self.live[self.row]
    }

    fn print(&mut self, c: char) {
        if !self.enabled || self.detached {
            return;
        }
        let col = self.col;
        let line = self.line();
        if col < line.len() {
            line[col] = c;
        } else {
            line.resize(col, ' ');
            line.push(c);
        }
        self.col += 1;
    }

    fn execute(&mut self, byte: u8) {
        if !self.enabled {
            return;
        }
        match byte {
            b'\n' | 0x0b | 0x0c => {
                if self.detached {
                    self.detached = false;
                    self.saved = None;
                    self.row = self.live.len();
                } else {
                    self.row += 1;
                }
                self.col = 0;
                self.line();
                self.retire();
            }
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => self.print('\t'),
            _ => {}
        }
    }

    fn csi(&mut self, params: &Params, action: char) {
        if !self.enabled {
            return;
        }
        let mut params = params
            .iter()
            .map(|p| p.first().copied().unwrap_or(0) as usize);
        let first = params.next().unwrap_or(0);
        let n = first.max(1);
        match action {
            'A' => self.row = self.row.saturating_sub(n),
            'B' => self.row += n,
            'C' => self.col += n,
            'D' => self.col = self.col.saturating_sub(n),
            'E' => (self.row, self.col) = (self.row + n, 0),
            'F' => (self.row, self.col) = (self.row.saturating_sub(n), 0),
            'G' | '`' => self.col = n - 1,
            'H' | 'f' => {
                if self.saved.is_some() {
                    self.detached = true;
                } else {
                    if !self.line().is_empty() {
                        self.row = self.live.len();
                    }
                    self.col = params.next().unwrap_or(1).max(1) - 1;
                }
            }
            'K' if !self.detached => {
                let col = self.col;
                let line = self.line();
                match first {
                    0 => line.truncate(col),
                    1 => line.iter_mut().take(col + 1).for_each(|c| *c = ' '),
                    2 => line.clear(),
                    _ => {}
                }
            }
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            _ => {}
        }
    }

    fn save_cursor(&mut self) {
        if self.enabled && !self.detached {
            self.saved = Some((self.row, self.col));
        }
    }

    fn restore_cursor(&mut self) {
        if let Some((row, col)) = self.saved.take() {
            (self.row, self.col, self.detached) = (row, col, false);
        }
    }

    /// Move lines the cursor can no longer reach out of `live`.
    fn retire(&mut self) {
        while self.row > LIVE_LINES {
            let Some(line) = self.live.pop_front() else {
                break;
            };
            self.done.extend(line);
            self.done.push('\n');
            self.row -= 1;
            if let Some((row, _)) = self.saved.as_mut() {
                *row = row.saturating_sub(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(bytes: &[u8]) -> String {
        let mut scanner = MarkScanner::new();
        scanner.text_mut().set_enabled(true);
        scanner.feed(bytes, |_, _| {});
        scanner.text_mut().take()
    }

    #[test]
    fn crlf_is_one_newline_and_colours_are_dropped() {
        assert_eq!(
            clean(b"\x1b[01;34mdir\x1b[0m  file\r\nnext\r\n"),
            "dir  file\nnext\n"
        );
    }

    #[test]
    fn carriage_return_progress_ends_as_its_last_state() {
        assert_eq!(clean(b"a\r b\r c\r\n"), " c\n");
        assert_eq!(clean(b"10%\r\x1b[K100%\r\ndone"), "100%\ndone");
        assert_eq!(clean(b"50%\r100%"), "100%");
    }

    #[test]
    fn cursor_up_redraws_replace_lines() {
        // A two-line live display redrawn once.
        let bytes = b"a 1\r\nb 1\r\n\x1b[2A\x1b[2Ka 2\r\n\x1b[2Kb 2\r\nend\r\n";
        assert_eq!(clean(bytes), "a 2\nb 2\nend\n");
    }

    #[test]
    fn status_line_drawn_elsewhere_is_dropped() {
        // apt's fancy progress: save, jump to the last row, draw, restore.
        let bytes =
            b"Unpacking x\r\n\x1b7\x1b[24;0f\x1b[42mProgress: [ 20%]\x1b[49m\x1b8Setting up x\r\n";
        assert_eq!(clean(bytes), "Unpacking x\nSetting up x\n");
    }

    #[test]
    fn clear_screen_continues_on_a_new_line() {
        assert_eq!(
            clean(b"before\r\n\x1b[H\x1b[2Jafter\r\n"),
            "before\nafter\n"
        );
    }

    #[test]
    fn backspace_overstrike_keeps_the_last_char() {
        assert_eq!(clean(b"N\x08NAME\r\n"), "NAME\n");
    }

    #[test]
    fn marks_split_the_text_across_chunks() {
        let stream: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07\x1b]6973;C;id1\x07out\r\n\x1b]133;D;3\x07\x1b]6973;D;id1;3\x07";
        let mut scanner = MarkScanner::new();
        let mut seen = Vec::new();
        // One byte at a time: sequences must survive any split.
        for byte in stream {
            scanner.feed(std::slice::from_ref(byte), |mark, text| {
                let before = text.take();
                text.set_enabled(true);
                seen.push((mark, before));
            });
        }
        let marks: Vec<_> = seen.iter().map(|(m, _)| m.clone()).collect();
        assert_eq!(
            marks,
            vec![
                Mark::PromptStart,
                Mark::CommandStart,
                Mark::OutputStart,
                Mark::Private(vec!["C".into(), "id1".into()]),
                Mark::CommandEnd { exit_code: Some(3) },
                Mark::Private(vec!["D".into(), "id1".into(), "3".into()]),
            ]
        );
        assert_eq!(seen[2].1, "ls\n", "typed command between B and C");
        assert_eq!(seen[4].1, "out\n", "output between C and D");
    }

    #[test]
    fn distant_lines_are_retired_but_kept() {
        let mut bytes = Vec::new();
        for i in 0..2_000 {
            bytes.extend_from_slice(format!("{i}\r\n").as_bytes());
        }
        let text = clean(&bytes);
        assert!(text.starts_with("0\n1\n"));
        assert!(text.ends_with("1999\n"));
        assert_eq!(text.lines().count(), 2_000);
    }
}
