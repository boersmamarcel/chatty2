//! Context the desktop composer adds to a user turn besides what was typed:
//! files dragged in from the file tree, and the artifact open in the viewer.
//!
//! A dragged file goes in with its text, a folder with its listing:
//!
//! ```text
//! <file_context path="src/main.rs" lines="120" bytes="3412">
//! …the file…
//! </file_context>
//! ```
//!
//! The artifact open in the viewer goes in as a reference only, so a message
//! like "make the second heading shorter" says which artifact it means:
//!
//! ```text
//! <artifact_open kind="file" path="docs/plan.md" bytes="…">
//! The user has this open in the viewer next to the chat.
//! </artifact_open>
//! ```
//!
//! Each block is its own text part of the user turn, after the typed text
//! and before a terminal snapshot, and is persisted with the message. The
//! desktop renders them as chips. `bytes` is the body's length, so a file
//! that itself contains these tags is still read back exactly.

use std::path::Path;

use rig_core::completion::Message;
use rig_core::completion::message::Text;
use rig_core::message::UserContent;

use crate::services::terminal::context::{attr, escape_attr, unescape_attr};
use crate::token_budget::counter::TokenCounter;

const FILE_TAG: &str = "file_context";
const OPEN_TAG: &str = "artifact_open";

/// Hard cap on one file's block, in tokens.
pub const FILE_TOKEN_CAP: usize = 8_000;
/// Most bytes read from a file before the token cap is applied.
const FILE_READ_BYTES: usize = 512 * 1024;
/// Most entries a folder's listing keeps.
const FOLDER_ENTRIES: usize = 200;
/// The line that says the file was cut.
pub const TRUNCATED_LINE: &str = "(truncated, use read_file for the rest)";
/// The body of an [`OpenArtifact`] block.
const OPEN_NOTE: &str = "The user has this open in the viewer next to the chat.";

/// One block, as built for sending or read back from a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBlock {
    pub kind: BlockKind,
    /// The path (or URL) the chip shows.
    pub target: String,
    /// The whole block, tags included: exactly what the model gets.
    pub block: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    File,
    Folder,
    /// The artifact open in the viewer.
    Open,
}

/// What the artifact viewer shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenArtifact {
    File {
        path: String,
    },
    Browser {
        url: String,
    },
    /// The session's changes, file by file.
    Review,
}

impl ContextBlock {
    /// A dragged file or folder at `path`, shown to the model as `shown`
    /// (relative to the workspace when it is inside it).
    pub fn for_path(path: &Path, shown: &str, counter: &TokenCounter) -> Self {
        if path.is_dir() {
            return Self::folder(path, shown);
        }
        let (body, mut attrs) = match read_text(path) {
            Ok(Some((text, cut))) => file_body(&text, cut, counter),
            Ok(None) => (
                "(not a text file, use read_binary)".to_string(),
                vec![("binary", "true".to_string())],
            ),
            Err(err) => (
                format!("(could not be read: {err})"),
                vec![("unreadable", "true".to_string())],
            ),
        };
        attrs.insert(0, ("path", shown.to_string()));
        Self {
            kind: BlockKind::File,
            target: shown.to_string(),
            block: assemble(FILE_TAG, &attrs, &body),
        }
    }

    /// A file `@`-mentioned in the typed text (AGE-2192): the same block as
    /// a dragged file, but only for a text file; `None` for a folder, a
    /// binary or an unreadable file, which stay a path in the text.
    pub fn for_mention(path: &Path, shown: &str, counter: &TokenCounter) -> Option<Self> {
        if !path.is_file() || !matches!(read_text(path), Ok(Some(_))) {
            return None;
        }
        Some(Self::for_path(path, shown, counter))
    }

    fn folder(path: &Path, shown: &str) -> Self {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        if e.path().is_dir() {
                            format!("{name}/")
                        } else {
                            name
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort_unstable();
        let total = names.len();
        names.truncate(FOLDER_ENTRIES);
        let mut body = names.join("\n");
        if total > FOLDER_ENTRIES {
            body.push_str(&format!(
                "\n({} more, use list_directory)",
                total - FOLDER_ENTRIES
            ));
        }
        let attrs = vec![
            ("path", shown.to_string()),
            ("folder", "true".to_string()),
            ("entries", total.to_string()),
        ];
        Self {
            kind: BlockKind::Folder,
            target: shown.to_string(),
            block: assemble(FILE_TAG, &attrs, &body),
        }
    }

    /// The reference to what the viewer shows.
    pub fn for_open(open: &OpenArtifact) -> Self {
        let (kind, key, target) = match open {
            OpenArtifact::File { path } => ("file", "path", path.clone()),
            OpenArtifact::Browser { url } => ("browser", "url", url.clone()),
            OpenArtifact::Review => ("review", "path", "session changes".to_string()),
        };
        let attrs = vec![("kind", kind.to_string()), (key, target.clone())];
        Self {
            kind: BlockKind::Open,
            target,
            block: assemble(OPEN_TAG, &attrs, OPEN_NOTE),
        }
    }

    /// The chip's text.
    pub fn label(&self) -> String {
        let name = match self.kind {
            BlockKind::Open => return format!("Viewing: {}", short_name(&self.target)),
            _ => short_name(&self.target),
        };
        match self.kind {
            BlockKind::Folder => format!("{name}/"),
            _ => name,
        }
    }
}

/// Split a user message's display text into what was typed and the context
/// blocks after it (in order). The terminal snapshot, if any, has to be
/// split off first.
pub fn split_composer_context(text: &str) -> (&str, Vec<ContextBlock>) {
    let mut start = 0;
    while let Some(found) = next_block_start(text, start) {
        if let Some(blocks) = parse_all(&text[found..]) {
            return (text[..found].trim_end_matches('\n'), blocks);
        }
        start = found + 1;
    }
    (text, Vec::new())
}

/// The first offset at or after `from` where a line opens one of the tags.
fn next_block_start(text: &str, from: usize) -> Option<usize> {
    let mut offset = from;
    while offset <= text.len() {
        let rest = &text[offset..];
        let hit = [FILE_TAG, OPEN_TAG]
            .iter()
            .filter_map(|tag| rest.find(&format!("<{tag} ")))
            .min()?;
        let at = offset + hit;
        if at == 0 || text.as_bytes()[at - 1] == b'\n' {
            return Some(at);
        }
        offset = at + 1;
    }
    None
}

/// Parse `text` as nothing but blocks separated by newlines.
fn parse_all(text: &str) -> Option<Vec<ContextBlock>> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while !rest.trim().is_empty() {
        let (block, len) = parse_one(rest)?;
        blocks.push(block);
        rest = rest[len..].trim_start_matches('\n');
    }
    (!blocks.is_empty()).then_some(blocks)
}

/// One block at the start of `text`, and its length.
fn parse_one(text: &str) -> Option<(ContextBlock, usize)> {
    let tag = [FILE_TAG, OPEN_TAG]
        .into_iter()
        .find(|tag| text.starts_with(&format!("<{tag} ")))?;
    let header_end = text.find(">\n")?;
    let attrs = &text[tag.len() + 2..header_end];
    let bytes: usize = attr(attrs, "bytes")?.parse().ok()?;
    let body_start = header_end + 2;
    let body_end = body_start.checked_add(bytes)?;
    let close = format!("\n</{tag}>");
    if text.get(body_end..)?.starts_with(&close) {
        let len = body_end + close.len();
        let (kind, target) = if tag == OPEN_TAG {
            let target = attr(attrs, "path").or_else(|| attr(attrs, "url"))?;
            (BlockKind::Open, target)
        } else if attr(attrs, "folder") == Some("true") {
            (BlockKind::Folder, attr(attrs, "path")?)
        } else {
            (BlockKind::File, attr(attrs, "path")?)
        };
        Some((
            ContextBlock {
                kind,
                target: unescape_attr(target),
                block: text[..len].to_string(),
            },
            len,
        ))
    } else {
        None
    }
}

/// Whether a user text part is a dragged file's or folder's block.
pub fn is_file_context(text: &str) -> bool {
    matches!(parse_one(text), Some((block, len)) if block.kind != BlockKind::Open && len == text.len())
}

/// Replace the file blocks of every user message but the newest one that
/// carries any with a one-line stub; the context shaper's first stage,
/// with the terminal snapshots. The newest message's files are what the
/// user is asking about; older ones can be read again. Returns the indices
/// changed.
pub fn stub_older_file_contexts(messages: &mut [Message]) -> Vec<usize> {
    let mut seen_newest = false;
    let mut changed = Vec::new();
    for (index, message) in messages.iter_mut().enumerate().rev() {
        let Message::User { content } = message else {
            continue;
        };
        let carries = content
            .iter()
            .any(|part| matches!(part, UserContent::Text(text) if is_file_context(&text.text)));
        if !carries {
            continue;
        }
        if !seen_newest {
            seen_newest = true;
            continue;
        }
        for part in content.iter_mut() {
            let UserContent::Text(text) = part else {
                continue;
            };
            let Some((block, _)) = parse_one(&text.text).filter(|_| is_file_context(&text.text))
            else {
                continue;
            };
            *part = UserContent::Text(Text::new(format!(
                "[file \"{}\" attached earlier, removed to save context; read it again if needed]",
                block.target
            )));
        }
        changed.push(index);
    }
    changed.reverse();
    changed
}

/// `path` relative to `root` when it is inside it, else in full.
pub fn display_path(path: &Path, root: Option<&Path>) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .filter(|rel| !rel.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn assemble(tag: &str, attrs: &[(&str, String)], body: &str) -> String {
    let mut header = String::new();
    for (key, value) in attrs {
        header.push_str(&format!(" {key}=\"{}\"", escape_attr(value)));
    }
    format!("<{tag}{header} bytes=\"{}\">\n{body}\n</{tag}>", body.len())
}

/// The file's text (the first [`FILE_READ_BYTES`]) and whether it was cut
/// there, `None` when it is not UTF-8 text.
fn read_text(path: &Path) -> std::io::Result<Option<(String, bool)>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(FILE_READ_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let cut = bytes.len() > FILE_READ_BYTES;
    bytes.truncate(FILE_READ_BYTES);
    if bytes.contains(&0) {
        return Ok(None);
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // Cut inside a character: keep the valid prefix.
        Err(err) if cut && err.utf8_error().error_len().is_none() => {
            let valid = err.utf8_error().valid_up_to();
            let mut bytes = err.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
        Err(_) => return Ok(None),
    };
    Ok(Some((text, cut)))
}

/// The block body for `text` and its extra attributes: whole lines from the
/// start, at most [`FILE_TOKEN_CAP`] tokens. `cut`: the read stopped short.
fn file_body(
    text: &str,
    cut: bool,
    counter: &TokenCounter,
) -> (String, Vec<(&'static str, String)>) {
    let mut truncated = cut;
    let text = text.trim_end_matches('\n');
    let total = text.lines().count();
    let mut kept: Vec<&str> = text.lines().collect();
    if truncated {
        // The last line may be cut mid-way.
        kept.pop();
    }
    let render = |kept: &[&str], truncated: bool| {
        let mut body = kept.join("\n");
        if truncated {
            if !body.is_empty() {
                body.push('\n');
            }
            body.push_str(TRUNCATED_LINE);
        }
        body
    };
    let mut body = render(&kept, truncated);
    while counter.count(&body) > FILE_TOKEN_CAP && !kept.is_empty() {
        truncated = true;
        // Drop a tenth of what is left (at least one line) per round.
        let drop = (kept.len() / 10).max(1);
        kept.truncate(kept.len() - drop);
        body = render(&kept, truncated);
    }
    let mut attrs = vec![("lines", kept.len().to_string())];
    if truncated {
        attrs.push(("of", total.to_string()));
    }
    (body, attrs)
}

fn short_name(target: &str) -> String {
    let trimmed = target.trim_end_matches('/');
    if trimmed.contains("://") {
        return trimmed.to_string();
    }
    Path::new(trimmed)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| target.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counter() -> TokenCounter {
        TokenCounter::for_model("")
    }

    #[test]
    fn a_file_block_carries_its_text_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "# Plan\n\nship it\n").unwrap();
        let block = ContextBlock::for_path(&path, "notes.md", &counter());
        assert_eq!(block.kind, BlockKind::File);
        assert!(
            block
                .block
                .starts_with("<file_context path=\"notes.md\" lines=\"3\"")
        );
        assert!(block.block.contains("\n# Plan\n\nship it\n</file_context>"));
        assert_eq!(block.label(), "notes.md");

        let text = format!("what does this say?\n{}", block.block);
        let (typed, blocks) = split_composer_context(&text);
        assert_eq!(typed, "what does this say?");
        assert_eq!(blocks, vec![block]);
    }

    #[test]
    fn a_file_that_contains_the_tags_still_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tricky.txt");
        std::fs::write(
            &path,
            "<file_context path=\"x\" bytes=\"1\">\nx\n</file_context>\n<artifact_open kind=\"file\">\n",
        )
        .unwrap();
        let file = ContextBlock::for_path(&path, "tricky.txt", &counter());
        let open = ContextBlock::for_open(&OpenArtifact::File {
            path: "docs/plan.md".into(),
        });
        let text = format!("look\n{}\n{}", file.block, open.block);
        let (typed, blocks) = split_composer_context(&text);
        assert_eq!(typed, "look");
        assert_eq!(blocks, vec![file, open]);
    }

    #[test]
    fn a_folder_block_lists_its_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        let block = ContextBlock::for_path(dir.path(), "src", &counter());
        assert_eq!(block.kind, BlockKind::Folder);
        assert!(block.block.contains("folder=\"true\" entries=\"2\""));
        assert!(block.block.contains("\na.rs\nsub/\n</file_context>"));
        assert_eq!(block.label(), "src/");
    }

    #[test]
    fn a_large_file_is_cut_to_the_token_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let text: String = (0..20_000).map(|i| format!("line number {i}\n")).collect();
        std::fs::write(&path, text).unwrap();
        let block = ContextBlock::for_path(&path, "big.txt", &counter());
        assert!(counter().count(&block.block) <= FILE_TOKEN_CAP + 100);
        assert!(block.block.contains("of=\"20000\""));
        assert!(
            block
                .block
                .contains(&format!("\n{TRUNCATED_LINE}\n</file_context>"))
        );
        assert!(block.block.contains("\nline number 0\n"));
        let (_, blocks) = split_composer_context(&block.block);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn a_binary_file_is_not_inlined() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.bin");
        std::fs::write(&path, [0u8, 159, 146, 150]).unwrap();
        let block = ContextBlock::for_path(&path, "blob.bin", &counter());
        assert!(block.block.contains("binary=\"true\""));
        assert!(block.block.contains("use read_binary"));
    }

    #[test]
    fn the_open_artifact_is_a_reference() {
        let open = ContextBlock::for_open(&OpenArtifact::Browser {
            url: "https://example.com/a?b=\"c\"".into(),
        });
        assert!(open.block.starts_with(
            "<artifact_open kind=\"browser\" url=\"https://example.com/a?b=&quot;c&quot;\""
        ));
        assert!(open.block.contains(OPEN_NOTE));
        assert_eq!(open.label(), "Viewing: https://example.com/a?b=\"c\"");
        let (typed, blocks) = split_composer_context(&open.block);
        assert_eq!(typed, "");
        assert_eq!(blocks[0].target, "https://example.com/a?b=\"c\"");
    }

    #[test]
    fn older_file_blocks_are_stubbed_and_the_newest_message_keeps_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        std::fs::write(&path, "alpha\n").unwrap();
        let file = ContextBlock::for_path(&path, "a.md", &counter()).block;
        let open = ContextBlock::for_open(&OpenArtifact::Review).block;
        let user = |parts: Vec<String>| Message::User {
            content: parts
                .into_iter()
                .map(|p| UserContent::Text(Text::new(p)))
                .collect(),
        };
        let mut messages = vec![
            user(vec!["look".into(), file.clone(), open.clone()]),
            Message::assistant("ok"),
            user(vec!["and this".into(), file.clone()]),
            user(vec!["no files".into()]),
        ];
        assert_eq!(stub_older_file_contexts(&mut messages), vec![0]);
        let texts = |m: &Message| match m {
            Message::User { content } => content
                .iter()
                .filter_map(|p| match p {
                    UserContent::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let first = texts(&messages[0]);
        assert_eq!(first[0], "look");
        assert!(first[1].starts_with("[file \"a.md\" attached earlier"));
        assert_eq!(first[2], open, "the viewer reference is left alone");
        assert_eq!(texts(&messages[2])[1], file);
    }

    #[test]
    fn plain_text_has_no_blocks() {
        let text = "see <file_context path=\"a\"> in the docs";
        assert_eq!(split_composer_context(text), (text, Vec::new()));
    }

    #[test]
    fn paths_inside_the_workspace_are_relative() {
        let root = Path::new("/home/me/proj");
        assert_eq!(
            display_path(Path::new("/home/me/proj/src/a.rs"), Some(root)),
            "src/a.rs"
        );
        assert_eq!(
            display_path(Path::new("/etc/hosts"), Some(root)),
            "/etc/hosts"
        );
        assert_eq!(display_path(root, Some(root)), "/home/me/proj");
    }
}
