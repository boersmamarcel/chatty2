//! Command records from OSC 133 marks: each command's text, where its
//! output sits in the grid, and its exit code.
//!
//! The [`CommandTracker`] runs in the byte tap, on the PTY thread, before
//! alacritty's parser sees the bytes. It cannot read the real grid there:
//! the event loop holds the terminal lock across its reads, so locking it
//! from the tap would deadlock, and the grid would not have seen the bytes
//! yet anyway. So the tracker runs a second, shadow `Term` over the same
//! bytes and stops it at the exact byte where each mark ends. A mark in the
//! middle of a chunk gets the line the cursor is on right after the mark,
//! not the line at the start or end of the chunk.
//!
//! Lines are numbered from the first line the terminal ever showed:
//! `scrolled + row`, where `scrolled` counts the lines that have scrolled
//! off the top of the screen into history. Only the count is kept; the
//! shadow's history is dropped as it fills, so the shadow costs one screen
//! of memory. Line `n` is grid line `n - scrolled` in the real terminal
//! (negative lines are scrollback). When that is above the oldest line the
//! real terminal still keeps, the line has been evicted (scrollback limit,
//! `clear` with `\e[3J`, reset) and records pointing at it are `truncated`.
//!
//! A resize reflows the real grid; the tracker keeps the cursor's line
//! where it was and moves the count with it, so lines near the cursor stay
//! right and older records can be off by as many rows as the reflow
//! added or removed above them.

use std::collections::VecDeque;
use std::time::SystemTime;

use alacritty_terminal::Term;
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::{Config as TermConfig, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use crate::marks::{Mark, MarkScanner};

/// How many records are kept; older ones are dropped.
pub const MAX_RECORDS: usize = 200;

/// Most bytes of output [`Region::LastCommand`](crate::Region) returns; the
/// end is kept.
pub const LAST_COMMAND_OUTPUT_BYTES: usize = 16 * 1024;

/// The shadow is fed at most this many bytes at a time and its history is
/// counted and dropped in between, so its history never needs more rows.
const PIECE: usize = 1024;

/// One command run at an integrated shell's prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRecord {
    /// The command line as it ended up after editing, between the `B` and
    /// `C` marks. A multi-line command keeps its continuation prompts.
    pub command_text: String,
    /// Line (see the module docs) where output starts: the cursor's line at
    /// the `C` mark.
    pub output_start_line: u64,
    /// The cursor's line at the `D` mark; `None` while the command runs.
    pub end_line: Option<u64>,
    /// From `D;<code>`; `None` while running or when the shell sent none.
    pub exit_code: Option<i32>,
    pub started_at: SystemTime,
    pub finished_at: Option<SystemTime>,
    /// Some of the output's lines have left the terminal's scrollback.
    pub truncated: bool,
    output_start_column: usize,
    end_column: usize,
}

impl CommandRecord {
    pub fn is_running(&self) -> bool {
        self.end_line.is_none()
    }
}

/// Where the shell is, from its marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// Printing the prompt (after `A`).
    Prompt,
    /// Reading a command line (after `B`).
    Typing,
}

pub(crate) struct CommandTracker {
    scanner: MarkScanner,
    shadow: Term<VoidListener>,
    processor: Processor,
    /// Lines scrolled into history since the terminal started.
    scrolled: u64,
    phase: Phase,
    records: VecDeque<CommandRecord>,
    /// Some OSC 133 mark was seen: the shell is integrated.
    integrated: bool,
}

impl CommandTracker {
    pub(crate) fn new(size: &impl Dimensions) -> Self {
        let config = TermConfig {
            scrolling_history: PIECE,
            ..TermConfig::default()
        };
        Self {
            scanner: MarkScanner::new(),
            shadow: Term::new(config, size, VoidListener),
            processor: Processor::new(),
            scrolled: 0,
            phase: Phase::Idle,
            records: VecDeque::new(),
            integrated: false,
        }
    }

    /// Scan bytes read from the PTY, in order.
    pub(crate) fn feed(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let (used, mark) = self.scanner.next_mark(bytes);
            self.advance_shadow(&bytes[..used]);
            bytes = &bytes[used..];
            if let Some(mark) = mark {
                self.on_mark(mark);
            }
        }
    }

    fn advance_shadow(&mut self, bytes: &[u8]) {
        for piece in bytes.chunks(PIECE) {
            self.processor.advance(&mut self.shadow, piece);
            self.count_scrolled();
        }
    }

    /// Move the shadow's history into the count. The primary screen's
    /// history is only reachable while it is active; lines it gained before
    /// a switch to the alternate screen are counted on the way back.
    fn count_scrolled(&mut self) {
        if self.shadow.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let grid = self.shadow.grid_mut();
        self.scrolled += grid.history_size() as u64;
        grid.clear_history();
    }

    /// The cursor as (line, column) right now in the stream.
    fn cursor(&mut self) -> (u64, usize) {
        // Bytes held back by a synchronized update are the terminal's to
        // show later; where they leave the cursor is where the mark is.
        if self.processor.sync_bytes_count() > 0 {
            self.processor.stop_sync(&mut self.shadow);
            self.count_scrolled();
        }
        let point = self.shadow.grid().cursor.point;
        (self.scrolled + point.line.0.max(0) as u64, point.column.0)
    }

    fn running(&mut self) -> Option<&mut CommandRecord> {
        self.records.back_mut().filter(|r| r.is_running())
    }

    fn on_mark(&mut self, mark: Mark) {
        let text = self.scanner.text_mut();
        match mark {
            Mark::PromptStart => {
                self.integrated = true;
                self.phase = Phase::Prompt;
                // Keep the prompt, to know where the command line starts.
                text.set_enabled(false);
                text.set_enabled(true);
            }
            Mark::CommandStart => {
                self.integrated = true;
                self.phase = Phase::Typing;
                // Keep what is typed from here, starting after the prompt:
                // line editing redraws relative to the prompt's width.
                let shown = text.take();
                let width = shown.rsplit('\n').next().unwrap_or("").chars().count();
                text.set_enabled(true);
                text.move_to_column(width);
            }
            Mark::OutputStart => {
                self.integrated = true;
                let typed = if self.phase == Phase::Typing {
                    text.take()
                } else {
                    String::new()
                };
                text.set_enabled(false);
                self.phase = Phase::Idle;
                // A second `C` before `D` (bash shows `PS0` once per command
                // of a multi-line command line) belongs to the same command.
                if self.running().is_some() {
                    return;
                }
                let (line, column) = self.cursor();
                if self.records.len() == MAX_RECORDS {
                    self.records.pop_front();
                }
                self.records.push_back(CommandRecord {
                    command_text: typed.trim().to_string(),
                    output_start_line: line,
                    end_line: None,
                    exit_code: None,
                    started_at: SystemTime::now(),
                    finished_at: None,
                    truncated: false,
                    output_start_column: column,
                    end_column: 0,
                });
            }
            Mark::CommandEnd { exit_code } => {
                self.integrated = true;
                text.set_enabled(false);
                self.phase = Phase::Idle;
                // `D` also comes at prompts where nothing ran (an empty
                // line, the first prompt); only a running command ends.
                if self.running().is_none() {
                    return;
                }
                let (line, column) = self.cursor();
                if let Some(record) = self.running() {
                    record.end_line = Some(line);
                    record.end_column = column;
                    record.exit_code = exit_code;
                    record.finished_at = Some(SystemTime::now());
                }
            }
            Mark::Private(_) => {}
        }
    }

    /// Resize the real terminal and the shadow together. Call with the
    /// terminal's lease held, so no bytes are between the tap and the
    /// parser.
    pub(crate) fn resize<T>(&mut self, term: &mut Term<T>, size: impl Dimensions + Copy) {
        let alt = term.mode().contains(TermMode::ALT_SCREEN);
        let cursor_line = self.scrolled as i64 + i64::from(term.grid().cursor.point.line.0);
        term.resize(size);
        self.shadow.resize(size);
        if !alt {
            // Anchor at the cursor: its line keeps its number, whatever the
            // reflow did above it.
            let cursor = &term.grid().cursor;
            let (point, wrap) = (cursor.point, cursor.input_needs_wrap);
            let shadow = &mut self.shadow.grid_mut().cursor;
            shadow.point = point;
            shadow.input_needs_wrap = wrap;
            self.shadow.grid_mut().clear_history();
            self.scrolled = (cursor_line - i64::from(point.line.0)).max(0) as u64;
        }
    }

    /// Mark records whose output has left `term`'s scrollback. Call with
    /// the lease held (see [`resize`](Self::resize)).
    fn refresh<T>(&mut self, term: &Term<T>) {
        // On the alternate screen the primary's history can't be read;
        // nothing is evicted from it meanwhile.
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        let oldest = self.oldest_line(term);
        for record in &mut self.records {
            if (record.output_start_line as i64) < oldest {
                record.truncated = true;
            }
        }
    }

    /// Number of the oldest line `term` still keeps.
    fn oldest_line<T>(&self, term: &Term<T>) -> i64 {
        self.scrolled as i64 - term.history_size() as i64
    }

    pub(crate) fn records<T>(&mut self, term: &Term<T>) -> Vec<CommandRecord> {
        self.refresh(term);
        self.records.iter().cloned().collect()
    }

    /// The last command and its output, read from `term`.
    pub(crate) fn last_command<T>(&mut self, term: &Term<T>) -> (LastCommand, String) {
        self.refresh(term);
        let Some(record) = self.records.back().cloned() else {
            return if self.integrated {
                (LastCommand::NoCommandYet, "no command has run yet".into())
            } else {
                (
                    LastCommand::NotActive,
                    "shell integration not active: this shell sends no OSC 133 marks".into(),
                )
            };
        };
        let (output, output_capped) = if term.mode().contains(TermMode::ALT_SCREEN) {
            // A full-screen program is running; its output is the screen.
            (String::new(), false)
        } else {
            self.output(term, &record)
        };
        (
            LastCommand::Ran {
                record,
                output_capped,
            },
            output,
        )
    }

    /// The record's output as plain text, and whether it was capped.
    fn output<T>(&self, term: &Term<T>, record: &CommandRecord) -> (String, bool) {
        let to_grid = |line: u64| Line((line as i64 - self.scrolled as i64) as i32);
        let top = term.topmost_line();
        let mut start = Point::new(
            to_grid(record.output_start_line),
            Column(record.output_start_column),
        );
        if start.line < top {
            start = Point::new(top, Column(0));
        }
        let end = match record.end_line {
            // `D` sits right after the output; the cell before it is its end.
            Some(line) if record.end_column > 0 => {
                Point::new(to_grid(line), Column(record.end_column - 1))
            }
            Some(line) => Point::new(to_grid(line) - 1, term.last_column()),
            None => Point::new(term.grid().cursor.point.line, term.last_column()),
        };
        if end < start {
            return (String::new(), false);
        }
        let text = term.bounds_to_string(start, end);
        let text = text.trim_end();
        if text.len() <= LAST_COMMAND_OUTPUT_BYTES {
            return (text.to_string(), false);
        }
        let mut cut = text.len() - LAST_COMMAND_OUTPUT_BYTES;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        // Start at a line boundary when one is near.
        if let Some(newline) = text[cut..].find('\n') {
            cut += newline + 1;
        }
        (text[cut..].to_string(), true)
    }
}

/// What [`Region::LastCommand`](crate::Region::LastCommand) found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastCommand {
    /// No OSC 133 mark was ever seen: the shell has no integration (`sh`,
    /// integration turned off, an unsupported shell). Not an error.
    NotActive,
    /// The shell is integrated but no command has run yet.
    NoCommandYet,
    /// The last command; the snapshot's text is its output (empty while a
    /// full-screen program runs).
    Ran {
        record: CommandRecord,
        /// The output was longer than [`LAST_COMMAND_OUTPUT_BYTES`] and
        /// only its end is returned.
        output_capped: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GridSize;

    fn tracker() -> CommandTracker {
        CommandTracker::new(&GridSize { cols: 20, rows: 5 })
    }

    const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";

    #[test]
    fn a_mark_mid_chunk_gets_the_line_at_the_mark() {
        let mut t = tracker();
        // One chunk: prompt, command, two output lines, end, next prompt.
        // `C` is on line 1 and `D` on line 3; the chunk ends on line 3's
        // prompt, and later lines scroll the screen.
        let mut chunk = Vec::new();
        chunk.extend_from_slice(PROMPT);
        chunk.extend_from_slice(b"ls\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;0\x07");
        chunk.extend_from_slice(PROMPT);
        t.feed(&chunk);
        let r = &t.records[0];
        assert_eq!(r.command_text, "ls");
        assert_eq!((r.output_start_line, r.output_start_column), (1, 0));
        assert_eq!((r.end_line, r.end_column), (Some(3), 0));
        assert_eq!(r.exit_code, Some(0));

        // Same bytes one at a time: same lines.
        let mut u = tracker();
        for byte in &chunk {
            u.feed(std::slice::from_ref(byte));
        }
        let lines = |t: &CommandTracker| {
            t.records
                .iter()
                .map(|r| (r.output_start_line, r.end_line, r.end_column))
                .collect::<Vec<_>>()
        };
        assert_eq!(lines(&u), lines(&t));
    }

    #[test]
    fn lines_keep_counting_once_the_screen_scrolls() {
        let mut t = tracker();
        // 12 lines on a 5-row screen, then a command.
        t.feed(&b"x\r\n".repeat(12));
        t.feed(PROMPT);
        t.feed(b"true\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
        let r = &t.records[0];
        assert_eq!(r.output_start_line, 13);
        assert_eq!(r.end_line, Some(13));
        assert_eq!(t.scrolled, 9);
    }

    #[test]
    fn end_without_start_and_repeated_start_are_ignored() {
        let mut t = tracker();
        t.feed(b"\x1b]133;D;0\x07");
        assert!(t.records.is_empty());
        t.feed(PROMPT);
        t.feed(b"a\r\n\x1b]133;C\x07\x1b]133;C\x07out\r\n\x1b]133;D;2\x07\x1b]133;D;0\x07");
        assert_eq!(t.records.len(), 1);
        assert_eq!(t.records[0].exit_code, Some(2));
    }

    #[test]
    fn the_agent_shell_marks_make_records_too() {
        // chatty-core's agent shell (AGE-585): its prompt, an agent command
        // with its id marks, then a command typed at the prompt.
        let mut t = tracker();
        t.feed(b"\x1b]6973;ready\x07\x1b]133;D;0\x07\x1b]133;A\x07~$ \x1b]133;B\x07");
        t.feed(
            b"ls\r\n\x1b]133;C\x07\x1b]6973;C;id1\x07out\r\n\x1b]133;D;0\x07\x1b]6973;D;id1;0\x07",
        );
        t.feed(b"\x1b]133;A\x07~$ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        let got: Vec<_> = t
            .records
            .iter()
            .map(|r| (r.command_text.as_str(), r.exit_code))
            .collect();
        assert_eq!(got, vec![("ls", Some(0)), ("false", Some(1))]);
    }

    #[test]
    fn the_ring_keeps_the_last_records() {
        let mut t = tracker();
        for i in 0..MAX_RECORDS + 5 {
            t.feed(PROMPT);
            t.feed(format!("c{i}\r\n\x1b]133;C\x07\x1b]133;D;0\x07").as_bytes());
        }
        assert_eq!(t.records.len(), MAX_RECORDS);
        assert_eq!(t.records[0].command_text, "c5");
    }
}
