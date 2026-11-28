//! Shared `@` file-mention picker helpers for the chat input, used by both
//! the GPUI desktop app and the terminal UI.
//!
//! Pure text/filesystem helpers only (AGE-172) — UI framework code
//! (rendering, focus, keyboard handling) stays in each frontend.
//!
//! The picker offers every file and folder under the working folder, as git
//! sees it (AGE-2192): `.gitignore` and hidden entries are left out. Folders
//! end in `/`. Typing ranks the paths fuzzily; a query that ends in `/` lists
//! that folder instead, so choosing a folder walks into it. On send, a
//! mentioned text file of at most [`MENTION_INLINE_BYTES`] goes along as a
//! `<file_context>` block, as a dragged file does.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::services::composer_context::ContextBlock;
use crate::token_budget::counter::TokenCounter;

/// Directories and files excluded from the @ mention file list, on top of
/// what `.gitignore` leaves out (a folder that is not a git repository has
/// none).
const AT_EXCLUDED: &[&str] = &[
    "node_modules",
    "target",
    "__pycache__",
    "dist",
    "build",
    ".git",
];

/// Maximum number of @ mention items shown in the picker.
pub const AT_MENU_MAX_ITEMS: usize = 50;

/// Most entries the picker indexes, so a huge folder (a home directory)
/// cannot stall the input.
pub const AT_INDEX_MAX_ENTRIES: usize = 100_000;

/// Longest the index walk may take; what was found by then is the index.
/// A walk of a large folder on a slow disk can take many seconds.
pub const AT_INDEX_MAX_TIME: Duration = Duration::from_secs(3);

/// A mentioned file at most this large goes along with the message.
pub const MENTION_INLINE_BYTES: u64 = 50 * 1024;

/// Every file and folder under `dir` for the `@` picker, as relative paths
/// with `/` separators; folders end in `/`. Respects `.gitignore` (also
/// outside a git repository), skips hidden entries and common
/// build/dependency directories, and stops at [`AT_INDEX_MAX_ENTRIES`] or
/// [`AT_INDEX_MAX_TIME`]. Sorted shallow first, then by path, so an empty
/// query shows the top. Blocking: a UI runs it off its event loop.
pub fn load_files_for_dir(dir: &Path) -> Vec<String> {
    let started = Instant::now();
    let walker = ignore::WalkBuilder::new(dir)
        .hidden(true)
        .require_git(false)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !AT_EXCLUDED.contains(&entry.file_name().to_string_lossy().as_ref())
        })
        .build();
    let mut files: Vec<String> = Vec::new();
    for entry in walker.filter_map(Result::ok) {
        if entry.depth() == 0 {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(dir) else {
            continue;
        };
        let mut rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            rel.push('/');
        }
        files.push(rel);
        if files.len() >= AT_INDEX_MAX_ENTRIES || started.elapsed() > AT_INDEX_MAX_TIME {
            break;
        }
    }
    files.sort_unstable_by(|a, b| depth(a).cmp(&depth(b)).then_with(|| a.cmp(b)));
    files
}

/// How many folders deep an entry is (`a/b.rs` and `a/b/` are both 1).
fn depth(entry: &str) -> usize {
    entry.trim_end_matches('/').matches('/').count()
}

/// The entry's last component, without a folder's `/`.
fn file_name(entry: &str) -> &str {
    let trimmed = entry.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// Extract the `@` query from the end of `input_text`.
///
/// Returns `Some(query)` when the text ends with `@<word>` (no whitespace
/// after `@`). Returns `None` when no `@` is present or when there is
/// whitespace after the `@`.
pub fn at_query_from(input_text: &str) -> Option<String> {
    // Work on the trailing portion of the text (handle multiline gracefully).
    let last_line = input_text.lines().next_back().unwrap_or(input_text);
    let at_pos = last_line.rfind('@')?;
    let after_at = &last_line[at_pos + 1..];
    // Close the menu as soon as the user types a space (including trailing).
    if after_at.chars().any(char::is_whitespace) {
        return None;
    }
    Some(after_at.to_ascii_lowercase())
}

/// Return the subset of `files` that match the current `@` query in
/// `input_text`, best first, capped at [`AT_MENU_MAX_ITEMS`].
///
/// - An empty query: the entries in index order (the top of the folder).
/// - A query ending in `/` that names a folder: that folder's own entries.
/// - Otherwise a fuzzy match on the whole path; a match in the file name
///   ranks above one only in the folders, then shorter paths first.
pub fn at_menu_items_for<'a>(input_text: &str, files: &'a [String]) -> Vec<&'a String> {
    let Some(query) = at_query_from(input_text) else {
        return Vec::new();
    };
    ranked(&query, files)
        .into_iter()
        .map(|i| &files[i])
        .collect()
}

/// The indices in `files` of the items for `query`, best first (see
/// [`at_menu_items_for`]).
fn ranked(query: &str, files: &[String]) -> Vec<usize> {
    if query.is_empty() {
        return (0..files.len().min(AT_MENU_MAX_ITEMS)).collect();
    }
    if query.ends_with('/') {
        let children: Vec<usize> = files
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.len() > query.len()
                    && f.is_char_boundary(query.len())
                    && f[..query.len()].eq_ignore_ascii_case(query)
                    && !f[query.len()..].trim_end_matches('/').contains('/')
            })
            .map(|(i, _)| i)
            .take(AT_MENU_MAX_ITEMS)
            .collect();
        if !children.is_empty() {
            return children;
        }
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            let path_score = pattern.score(Utf32Str::new(f, &mut buf), &mut matcher)?;
            let name_score = pattern
                .score(Utf32Str::new(file_name(f), &mut buf), &mut matcher)
                .unwrap_or(0);
            Some((path_score + name_score, i))
        })
        .collect();
    let key = |&(score, i): &(u32, usize)| (Reverse(score), files[i].len(), files[i].as_str());
    scored.sort_unstable_by(|a, b| key(a).cmp(&key(b)));
    scored
        .into_iter()
        .take(AT_MENU_MAX_ITEMS)
        .map(|(_, i)| i)
        .collect()
}

/// The picker's index with the last query's result kept, so the several
/// lookups one keystroke makes (open?, how many?, render) rank once.
#[derive(Debug, Default)]
pub struct AtFiles {
    files: Vec<String>,
    /// The last query and the indices of its items in `files`.
    last: RefCell<Option<(String, Vec<usize>)>>,
}

impl AtFiles {
    pub fn new(files: Vec<String>) -> Self {
        Self {
            files,
            last: RefCell::new(None),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// [`at_menu_items_for`] over this index.
    pub fn items_for(&self, input_text: &str) -> Vec<&String> {
        let Some(query) = at_query_from(input_text) else {
            return Vec::new();
        };
        let mut last = self.last.borrow_mut();
        if last.as_ref().is_none_or(|(q, _)| *q != query) {
            let hits = ranked(&query, &self.files);
            *last = Some((query, hits));
        }
        let hits = last
            .as_ref()
            .map(|(_, hits)| hits.as_slice())
            .unwrap_or_default();
        hits.iter().map(|&i| &self.files[i]).collect()
    }
}

/// Build the replacement input text when a file is chosen from the `@`
/// mention picker. The `@<query>` suffix is replaced with `@<filename> `.
/// A path with whitespace is wrapped in backticks. A folder gets no space
/// after it, so the picker stays open and lists what is inside.
pub fn apply_at_to_input(input_text: &str, filename: &str) -> String {
    let input_text = input_text.trim_end_matches(['\r', '\n']);
    let mention = if filename.chars().any(char::is_whitespace) {
        format!("`{filename}` ")
    } else if filename.ends_with('/') {
        filename.to_string()
    } else {
        format!("{filename} ")
    };
    // Find the `@` that opened the menu on the last line.
    let last_line_start = input_text.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let last_line = &input_text[last_line_start..];
    let Some(at_pos_in_line) = last_line.rfind('@') else {
        return format!("{} @{mention}", input_text.trim_end());
    };
    let before = &input_text[..last_line_start + at_pos_in_line];
    format!("{before}@{mention}")
}

/// The paths `@`-mentioned in `message`, as typed, in order, each once. A
/// mention starts at an `@` at the start of the text or after whitespace or
/// `(` (so an e-mail address is none), and runs to the next whitespace, or
/// to the closing backtick of ``@`a path with spaces` ``. Trailing
/// punctuation is not part of it.
pub fn mentioned_paths(message: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut prev: Option<char> = None;
    for (i, c) in message.char_indices() {
        let starts = c == '@' && prev.is_none_or(|p| p.is_whitespace() || p == '(');
        prev = Some(c);
        if !starts {
            continue;
        }
        let rest = &message[i + 1..];
        let path = if let Some(quoted) = rest.strip_prefix('`') {
            quoted.find('`').map(|end| &quoted[..end])
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            Some(rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '\'']))
        };
        if let Some(path) = path.filter(|p| !p.is_empty())
            && !found.iter().any(|f| f == path)
        {
            found.push(path.to_string());
        }
    }
    found
}

/// The `<file_context>` blocks for the files `message` mentions (AGE-2192):
/// each text file of at most [`MENTION_INLINE_BYTES`] inside `root`. A
/// folder, a larger or binary file, a path that leaves `root` (also through
/// a symlink) and a path that is not there stay text only. Files in `skip`
/// (already dragged in) are left out.
pub fn mention_blocks(message: &str, root: &Path, skip: &[PathBuf]) -> Vec<ContextBlock> {
    let Ok(root) = root.canonicalize() else {
        return Vec::new();
    };
    let skip: Vec<PathBuf> = skip.iter().filter_map(|p| p.canonicalize().ok()).collect();
    let counter = TokenCounter::for_model("");
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut blocks = Vec::new();
    for rel in mentioned_paths(message) {
        let Ok(path) = root.join(&rel).canonicalize() else {
            continue;
        };
        if !path.starts_with(&root) || skip.contains(&path) || seen.contains(&path) {
            continue;
        }
        let small =
            std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() <= MENTION_INLINE_BYTES);
        if !small {
            continue;
        }
        let shown = path
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or(rel);
        if let Some(block) = ContextBlock::for_mention(&path, &shown, &counter) {
            seen.push(path);
            blocks.push(block);
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for p in paths {
            let full = dir.path().join(p);
            if p.ends_with('/') {
                std::fs::create_dir_all(&full).unwrap();
            } else {
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(&full, format!("contents of {p}\n")).unwrap();
            }
        }
        dir
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn at_query_returns_none_when_no_at() {
        assert!(at_query_from("hello world").is_none());
        assert!(at_query_from("").is_none());
    }

    #[test]
    fn at_query_returns_query_after_at() {
        assert_eq!(at_query_from("@"), Some(String::new()));
        assert_eq!(at_query_from("@readme"), Some("readme".into()));
        assert_eq!(at_query_from("hello @src"), Some("src".into()));
    }

    #[test]
    fn at_query_closes_on_space() {
        assert!(at_query_from("@readme ").is_none());
        assert!(at_query_from("@readme.md and more").is_none());
    }

    #[test]
    fn load_walks_every_depth_and_marks_folders() {
        let dir = tree(&["README.md", "crates/core/src/lib.rs", "docs/"]);
        let files = load_files_for_dir(dir.path());
        assert_eq!(
            files,
            strings(&[
                "README.md",
                "crates/",
                "docs/",
                "crates/core/",
                "crates/core/src/",
                "crates/core/src/lib.rs",
            ])
        );
    }

    #[test]
    fn load_skips_gitignored_hidden_and_build_dirs() {
        let dir = tree(&[
            "src/main.rs",
            "secret.env",
            "logs/today.log",
            ".hidden/x.md",
            "node_modules/pkg/index.js",
            "target/debug/app",
        ]);
        std::fs::write(dir.path().join(".gitignore"), "*.env\nlogs/\n").unwrap();
        let files = load_files_for_dir(dir.path());
        assert_eq!(files, strings(&["src/", "src/main.rs"]));
    }

    #[test]
    fn empty_query_shows_the_top_first() {
        let files = strings(&["README.md", "src/", "src/main.rs"]);
        assert_eq!(
            at_menu_items_for("@", &files),
            vec!["README.md", "src/", "src/main.rs"]
        );
    }

    #[test]
    fn fuzzy_query_finds_deep_files() {
        let files = strings(&[
            "README.md",
            "crates/",
            "crates/chatty-gpui/src/chatty/views/chat_input/mod.rs",
            "crates/chatty-gpui/src/chatty/views/chat_input/render.rs",
        ]);
        let items = at_menu_items_for("@chinmod", &files);
        assert_eq!(items.first().map(|s| s.as_str()), Some(files[2].as_str()));
        assert!(at_menu_items_for("@zzz", &files).is_empty());
    }

    #[test]
    fn file_name_match_ranks_above_folder_match() {
        let files = strings(&["render/notes.txt", "src/ui/render.rs"]);
        assert_eq!(at_menu_items_for("@render", &files)[0], "src/ui/render.rs");
    }

    #[test]
    fn equal_matches_prefer_the_shorter_path() {
        let files = strings(&["a/b/c/lib.rs", "src/lib.rs"]);
        assert_eq!(at_menu_items_for("@lib.rs", &files)[0], "src/lib.rs");
    }

    #[test]
    fn folder_query_lists_that_folder() {
        let files = strings(&["src/", "src/ui/", "src/main.rs", "src/ui/app.rs", "srcx.md"]);
        assert_eq!(
            at_menu_items_for("@src/", &files),
            vec!["src/ui/", "src/main.rs"]
        );
        assert_eq!(
            at_menu_items_for("@SRC/", &files),
            vec!["src/ui/", "src/main.rs"]
        );
    }

    #[test]
    fn at_menu_is_capped() {
        let files: Vec<String> = (0..200).map(|i| format!("file{i}.rs")).collect();
        assert_eq!(at_menu_items_for("@file", &files).len(), AT_MENU_MAX_ITEMS);
    }

    #[test]
    fn at_files_matches_like_the_free_function_and_follows_the_query() {
        let files = AtFiles::new(strings(&["README.md", "src/", "src/main.rs"]));
        assert!(files.items_for("hello").is_empty());
        assert_eq!(files.items_for("@main"), vec!["src/main.rs"]);
        // The same query again is served from the kept result.
        assert_eq!(files.items_for("see @main"), vec!["src/main.rs"]);
        assert_eq!(files.items_for("@readme"), vec!["README.md"]);
        assert!(AtFiles::default().is_empty());
    }

    #[test]
    fn apply_at_to_input_replaces_query() {
        assert_eq!(apply_at_to_input("@", "README.md"), "@README.md ");
        assert_eq!(apply_at_to_input("@read", "README.md"), "@README.md ");
        assert_eq!(
            apply_at_to_input("please check @read", "README.md"),
            "please check @README.md "
        );
        assert_eq!(
            apply_at_to_input("one\ntwo @x", "a/b.rs"),
            "one\ntwo @a/b.rs "
        );
    }

    #[test]
    fn apply_folder_keeps_the_picker_open() {
        let text = apply_at_to_input("see @cr", "crates/");
        assert_eq!(text, "see @crates/");
        assert_eq!(at_query_from(&text), Some("crates/".into()));
    }

    #[test]
    fn apply_quotes_paths_with_spaces() {
        assert_eq!(apply_at_to_input("@my", "my notes.md"), "@`my notes.md` ");
    }

    #[test]
    fn mentioned_paths_parses_plain_quoted_and_punctuated() {
        assert_eq!(
            mentioned_paths("look at @src/main.rs, and @`my notes.md` (also @a.rs)."),
            strings(&["src/main.rs", "my notes.md", "a.rs"])
        );
        assert!(mentioned_paths("mail me at marcel@example.test").is_empty());
        assert_eq!(mentioned_paths("@a.rs @a.rs"), strings(&["a.rs"]));
    }

    #[test]
    fn mention_blocks_inline_small_text_files_only() {
        let dir = tree(&["src/main.rs", "docs/"]);
        std::fs::write(dir.path().join("big.txt"), "x".repeat(60 * 1024)).unwrap();
        std::fs::write(dir.path().join("bin.dat"), [0u8, 1, 2]).unwrap();
        let blocks = mention_blocks(
            "@src/main.rs @docs/ @big.txt @bin.dat @missing.rs @../escape.rs",
            dir.path(),
            &[],
        );
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].target, "src/main.rs");
        assert!(blocks[0].block.contains("contents of src/main.rs"));
        assert!(
            blocks[0]
                .block
                .starts_with("<file_context path=\"src/main.rs\"")
        );
    }

    #[test]
    fn mention_blocks_skip_dragged_files_and_duplicates() {
        let dir = tree(&["a.rs", "b.rs"]);
        let blocks = mention_blocks(
            "@a.rs @b.rs @./b.rs",
            dir.path(),
            &[dir.path().join("a.rs")],
        );
        assert_eq!(
            blocks.iter().map(|b| b.target.as_str()).collect::<Vec<_>>(),
            ["b.rs"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn mention_blocks_refuse_a_symlink_out_of_the_folder() {
        let outside = tree(&["secret.txt"]);
        let dir = tree(&[]);
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();
        assert!(mention_blocks("@link.txt", dir.path(), &[]).is_empty());
    }
}
