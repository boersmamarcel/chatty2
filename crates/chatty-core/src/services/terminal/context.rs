//! The terminal snapshot attached to a desktop user turn (AGE-587).
//!
//! When the desktop's terminal dock is open on a terminal the agent may read,
//! the composer attaches a compact snapshot of it to the message: the tab's
//! name and directory, then the last command with its exit code and the end
//! of its output (a shell with command marks), else the last visible lines.
//! It goes into the user turn as its own text part after what was typed,
//!
//! ```text
//! <terminal_context tab="bash — chatty2" lines="32">
//! cwd: /home/me/chatty2
//! command: cargo test
//! exit code: 101
//! output:
//! …
//! </terminal_context>
//! ```
//!
//! and is persisted with the message, so a replayed history is what the
//! model saw. The desktop renders it as a chip, not as text.
//!
//! This module only formats and recognises the block; the one caller that
//! builds one is the desktop dock at send time. Headless, `--pipe`,
//! chatty-tui, workers and hive never attach anything.
//!
//! A snapshot is attached only when it differs from the last one attached in
//! the conversation ([`to_attach`]), which is read back from the history
//! itself, so a long chat does not pay for the same screen every turn. The
//! context shaper stubs every block but the newest before anything else
//! when a request is over budget ([`stub_older_contexts`]).

use rig_core::completion::Message;
use rig_core::completion::message::Text;
use rig_core::message::UserContent;

use crate::token_budget::counter::TokenCounter;

/// Opening of the block's tag, up to its attributes.
const OPEN: &str = "<terminal_context ";
/// The block's closing tag.
const CLOSE: &str = "</terminal_context>";

/// Most output lines a snapshot keeps (the end is kept).
pub const CONTEXT_LINES: usize = 40;
/// Hard cap on a whole block, in tokens.
pub const CONTEXT_TOKEN_CAP: usize = 2_000;
/// The line that says lines were left out.
pub const TRUNCATED_LINE: &str = "(truncated, use terminal_read for more)";

/// The last command run at the terminal's prompt, from its command marks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCommandSummary {
    pub command: String,
    /// `None` while it runs, or when the shell sent none.
    pub exit_code: Option<i32>,
    pub running: bool,
}

/// What a snapshot is made of, as the dock reads it.
#[derive(Debug, Clone, Copy)]
pub struct TerminalContextInput<'a> {
    /// The tab's name as the dock shows it.
    pub tab: &'a str,
    pub cwd: Option<&'a str>,
    /// With a command: `text` is its output. Without: the visible screen.
    pub last_command: Option<&'a LastCommandSummary>,
    pub text: &'a str,
}

/// A snapshot block, ready to attach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalContext {
    pub tab: String,
    /// Output lines in the block.
    pub lines: usize,
    /// The whole block, tags included: exactly what the model gets.
    pub block: String,
}

impl TerminalContext {
    /// Build the block: ANSI-stripped, the last [`CONTEXT_LINES`] lines,
    /// and at most [`CONTEXT_TOKEN_CAP`] tokens by `counter`, with
    /// [`TRUNCATED_LINE`] where lines were left out.
    pub fn build(input: TerminalContextInput<'_>, counter: &TokenCounter) -> Self {
        let tab = one_line(&strip_ansi(input.tab));
        let mut header = String::new();
        if let Some(cwd) = input.cwd.filter(|cwd| !cwd.is_empty()) {
            header.push_str(&format!("cwd: {}\n", one_line(&strip_ansi(cwd))));
        }
        match input.last_command {
            Some(command) => {
                header.push_str(&format!(
                    "command: {}\n",
                    one_line(&strip_ansi(&command.command))
                ));
                let exit = match (command.running, command.exit_code) {
                    (true, _) => "still running".to_string(),
                    (false, Some(code)) => code.to_string(),
                    (false, None) => "unknown".to_string(),
                };
                header.push_str(&format!("exit code: {exit}\n"));
                header.push_str("output:\n");
            }
            None => header.push_str("screen:\n"),
        }

        let text = strip_ansi(input.text);
        let all: Vec<&str> = text.trim_end().lines().collect();
        let mut kept: Vec<String> = all
            .iter()
            .skip(all.len().saturating_sub(CONTEXT_LINES))
            .map(|line| line.to_string())
            .collect();
        let mut truncated = kept.len() < all.len();

        let assemble = |kept: &[String], truncated: bool| {
            let mut body = header.clone();
            if truncated {
                body.push_str(TRUNCATED_LINE);
                body.push('\n');
            }
            for line in kept {
                body.push_str(line);
                body.push('\n');
            }
            format!(
                "{OPEN}tab=\"{}\" lines=\"{}\">\n{body}{CLOSE}",
                escape_attr(&tab),
                kept.len()
            )
        };

        // Oldest lines go first; the newest line is cut from its start as a
        // last resort (one enormous line).
        let mut block = assemble(&kept, truncated);
        while counter.count(&block) > CONTEXT_TOKEN_CAP && !kept.is_empty() {
            truncated = true;
            if kept.len() > 1 {
                kept.remove(0);
            } else {
                let line = kept.remove(0);
                let count = line.chars().count();
                if count > 1 {
                    kept.push(line.chars().skip(count - count / 2).collect());
                }
            }
            block = assemble(&kept, truncated);
        }

        Self {
            tab,
            lines: kept.len(),
            block,
        }
    }

    /// The chip's text: `Terminal: bash — chatty2 · 32 lines`.
    pub fn label(&self) -> String {
        let unit = if self.lines == 1 { "line" } else { "lines" };
        format!("Terminal: {} · {} {unit}", self.tab, self.lines)
    }

    /// Read a block back (from a persisted message).
    pub fn parse(block: &str) -> Option<Self> {
        let block = block.trim();
        let rest = block.strip_prefix(OPEN)?;
        if !block.ends_with(CLOSE) {
            return None;
        }
        let tag_end = rest.find('>')?;
        let attrs = &rest[..tag_end];
        let tab = unescape_attr(attr(attrs, "tab")?);
        let lines = attr(attrs, "lines")?.parse().ok()?;
        Some(Self {
            tab,
            lines,
            block: block.to_string(),
        })
    }
}

/// Split a user message's display text into what was typed and the
/// snapshot attached after it, if any.
pub fn split_terminal_context(text: &str) -> (&str, Option<TerminalContext>) {
    let trimmed = text.trim_end();
    if !trimmed.ends_with(CLOSE) {
        return (text, None);
    }
    let Some(start) = trimmed.rfind(OPEN) else {
        return (text, None);
    };
    match TerminalContext::parse(&trimmed[start..]) {
        Some(context) => (trimmed[..start].trim_end_matches('\n'), Some(context)),
        None => (text, None),
    }
}

/// Whether a user text part is a snapshot block.
pub fn is_terminal_context(text: &str) -> bool {
    text.starts_with(OPEN) && text.trim_end().ends_with(CLOSE)
}

/// The newest snapshot block attached in `history`.
pub fn last_attached<'a>(history: impl DoubleEndedIterator<Item = &'a Message>) -> Option<&'a str> {
    history.rev().find_map(|message| match message {
        Message::User { content } => content.iter().rev().find_map(|part| match part {
            UserContent::Text(text) if is_terminal_context(&text.text) => Some(text.text.as_str()),
            _ => None,
        }),
        _ => None,
    })
}

/// `current`, unless it is the block the conversation already carries as
/// its last attached one: an unchanged terminal is not sent again.
pub fn to_attach<'a>(
    current: Option<TerminalContext>,
    history: impl DoubleEndedIterator<Item = &'a Message>,
) -> Option<TerminalContext> {
    current.filter(|context| last_attached(history) != Some(context.block.as_str()))
}

/// Replace every snapshot block in `messages` but the newest with a one-line
/// stub; the context shaper's first stage. Returns the indices changed.
pub fn stub_older_contexts(messages: &mut [Message]) -> Vec<usize> {
    let mut seen_newest = false;
    let mut changed = Vec::new();
    for (index, message) in messages.iter_mut().enumerate().rev() {
        let Message::User { content } = message else {
            continue;
        };
        let mut touched = false;
        for part in content.iter_mut().rev() {
            let UserContent::Text(text) = part else {
                continue;
            };
            if !is_terminal_context(&text.text) {
                continue;
            }
            if !seen_newest {
                seen_newest = true;
                continue;
            }
            let tab = TerminalContext::parse(&text.text)
                .map(|context| context.tab)
                .unwrap_or_default();
            *part = UserContent::Text(Text::new(format!(
                "[older terminal snapshot of \"{tab}\" removed to save context]"
            )));
            touched = true;
        }
        if touched {
            changed.push(index);
        }
    }
    changed.reverse();
    changed
}

/// Drop escape sequences (CSI, OSC, other ESC-prefixed) and control
/// characters other than newline and tab.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters and intermediates up to a final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC / DCS / APC / PM / SOS: up to BEL or ST.
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\n' | '\t' => out.push(c),
            '\r' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn escape_attr(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape_attr(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The value of `name="…"` in a tag's attributes.
fn attr<'a>(attrs: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let start = attrs.find(&key)? + key.len();
    let len = attrs[start..].find('"')?;
    Some(&attrs[start..start + len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter() -> TokenCounter {
        TokenCounter::for_model("")
    }

    fn screen(text: &str) -> TerminalContext {
        TerminalContext::build(
            TerminalContextInput {
                tab: "bash — chatty2",
                cwd: Some("/home/me/chatty2"),
                last_command: None,
                text,
            },
            &counter(),
        )
    }

    fn user(parts: &[&str]) -> Message {
        Message::User {
            content: parts
                .iter()
                .map(|p| UserContent::Text(Text::new(p.to_string())))
                .collect(),
        }
    }

    #[test]
    fn a_command_block_names_the_command_exit_code_and_output() {
        let command = LastCommandSummary {
            command: "cargo test".into(),
            exit_code: Some(101),
            running: false,
        };
        let context = TerminalContext::build(
            TerminalContextInput {
                tab: "bash — chatty2",
                cwd: Some("/home/me/chatty2"),
                last_command: Some(&command),
                text: "running 1 test\ntest x ... FAILED\n",
            },
            &counter(),
        );
        assert_eq!(
            context.block,
            "<terminal_context tab=\"bash — chatty2\" lines=\"2\">\n\
             cwd: /home/me/chatty2\n\
             command: cargo test\n\
             exit code: 101\n\
             output:\n\
             running 1 test\n\
             test x ... FAILED\n\
             </terminal_context>"
        );
        assert_eq!(context.label(), "Terminal: bash — chatty2 · 2 lines");
    }

    #[test]
    fn a_running_command_says_so() {
        let command = LastCommandSummary {
            command: "sleep 9".into(),
            exit_code: None,
            running: true,
        };
        let context = TerminalContext::build(
            TerminalContextInput {
                tab: "t",
                cwd: None,
                last_command: Some(&command),
                text: "",
            },
            &counter(),
        );
        assert!(context.block.contains("exit code: still running\n"));
        assert_eq!(context.lines, 0);
    }

    #[test]
    fn only_the_last_lines_are_kept_with_the_truncation_line() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let context = screen(&text);
        assert_eq!(context.lines, CONTEXT_LINES);
        assert!(
            context
                .block
                .contains(&format!("{TRUNCATED_LINE}\nline 60\n"))
        );
        assert!(context.block.contains("line 99\n</terminal_context>"));
        assert!(!context.block.contains("line 59\n"));
        // Short output: nothing left out, no truncation line.
        assert!(!screen("a\nb").block.contains(TRUNCATED_LINE));
    }

    #[test]
    fn the_token_cap_is_hard() {
        // 40 lines of ~100 tokens each: well over the cap.
        let long: String = (0..CONTEXT_LINES)
            .map(|i| format!("{i} {}\n", "word ".repeat(100)))
            .collect();
        let context = screen(&long);
        assert!(counter().count(&context.block) <= CONTEXT_TOKEN_CAP);
        assert!(context.lines < CONTEXT_LINES && context.lines > 0);
        assert!(context.block.contains(TRUNCATED_LINE));
        assert!(
            context
                .block
                .contains(&format!("{} word", CONTEXT_LINES - 1))
        );

        // One enormous line is cut from its start.
        let one = "x".repeat(40_000);
        let context = screen(&one);
        assert!(counter().count(&context.block) <= CONTEXT_TOKEN_CAP);
        assert!(context.block.contains(TRUNCATED_LINE));
    }

    #[test]
    fn escapes_and_control_characters_are_stripped() {
        let context = screen("\x1b[31mred\x1b[0m plain\r\n\x1b]0;title\x07bell\x07ok");
        assert!(context.block.contains("screen:\nred plain\nbellok\n"));
        assert_eq!(strip_ansi("a\x1b]133;D;0\x1b\\b\tc"), "ab\tc");
    }

    #[test]
    fn a_block_round_trips_through_the_display_text() {
        let context = screen("x");
        let text = format!("why did that fail?\n{}", context.block);
        let (typed, parsed) = split_terminal_context(&text);
        assert_eq!(typed, "why did that fail?");
        assert_eq!(parsed.as_ref(), Some(&context));
        // Quotes in a tab name survive.
        let odd = TerminalContext::build(
            TerminalContextInput {
                tab: "vim \"a<b>\"",
                cwd: None,
                last_command: None,
                text: "",
            },
            &counter(),
        );
        assert_eq!(
            TerminalContext::parse(&odd.block).unwrap().tab,
            "vim \"a<b>\""
        );
        // Plain text is left alone.
        assert_eq!(split_terminal_context("hello"), ("hello", None));
    }

    /// Same snapshot twice: attached once. A changed one: attached again.
    #[test]
    fn an_unchanged_snapshot_is_attached_once() {
        let mut history = vec![];
        let first = screen("error[E0425]");
        let attached = to_attach(Some(first.clone()), history.iter()).expect("first is new");
        history.push(user(&["why did that fail?", &attached.block]));
        history.push(Message::assistant("a typo"));

        assert_eq!(
            to_attach(Some(screen("error[E0425]")), history.iter()),
            None
        );
        history.push(user(&["and now?"]));
        history.push(Message::assistant("same"));
        assert_eq!(
            to_attach(Some(screen("error[E0425]")), history.iter()),
            None
        );

        let changed = screen("test result: ok");
        assert_eq!(
            to_attach(Some(changed.clone()), history.iter()),
            Some(changed)
        );
        assert_eq!(to_attach(None, history.iter()), None);
    }

    #[test]
    fn the_shaper_stubs_every_block_but_the_newest() {
        let old = screen("old");
        let new = screen("new");
        let mut messages = vec![
            user(&["first", &old.block]),
            Message::assistant("ok"),
            user(&["second", &new.block]),
        ];
        assert_eq!(stub_older_contexts(&mut messages), vec![0]);
        assert_eq!(
            last_attached(messages.iter()),
            Some(new.block.as_str()),
            "the newest block is whole"
        );
        let Message::User { content } = &messages[0] else {
            unreachable!()
        };
        let texts: Vec<_> = content
            .iter()
            .map(|c| match c {
                UserContent::Text(t) => t.text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                "first".to_string(),
                "[older terminal snapshot of \"bash — chatty2\" removed to save context]"
                    .to_string()
            ]
        );
        assert!(stub_older_contexts(&mut messages).is_empty());
    }
}
