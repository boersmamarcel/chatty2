//! The artifact panel's workspace explorer (AGE-476): the directory tree
//! model behind the left-hand file column and the file operations it offers.
//!
//! Pure filesystem and bookkeeping code; the pixels live in
//! `file_explorer.rs`. Directories are listed lazily on expand and re-listed
//! when their mtime moves, so a file the agent writes shows up without a
//! refresh button being pressed — a directory's mtime changes whenever an
//! entry is added, removed or renamed in it, which is exactly the set of
//! events the tree cares about.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long the tree trusts its listings before it stats the expanded
/// directories again. Polled from render, like the artifact's own
/// staleness check, so the cost is a handful of `stat`s per interval and
/// nothing while the panel is closed.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(2);

/// Never shown: the object store is noise in an explorer, and it is the one
/// directory whose churn would make every mtime poll re-list.
const HIDDEN_NAMES: &[&str] = &[".git"];

/// Cap for [`walk_files`] so a huge workspace (or a symlink cycle —
/// `list_dir` already follows symlinks for `is_dir`) cannot hang the
/// Cmd+P quick-open picker (AGE-480) walking it.
pub const MAX_QUICK_OPEN_FILES: usize = 20_000;

/// Every file under `root`, for the Cmd+P quick-open picker (AGE-480):
/// eagerly walks the whole tree — the picker needs it all up front, unlike
/// the lazily-expanded [`FileTree`] — applying the same [`HIDDEN_NAMES`]
/// rule via [`list_dir`]. Order is not guaranteed; the caller ranks and
/// sorts.
pub fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        if out.len() >= MAX_QUICK_OPEN_FILES {
            break;
        }
        for node in list_dir(&dir).entries {
            if node.is_dir {
                dirs.push(node.path);
            } else {
                out.push(node.path);
            }
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Default)]
struct DirListing {
    entries: Vec<TreeNode>,
    mtime: Option<SystemTime>,
}

/// An inline name entry in progress: a new entry under `dir`, or a rename
/// of `path`. The row it occupies is rendered as an input instead of a
/// label until it is committed or cancelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingEdit {
    NewFile { dir: PathBuf },
    NewFolder { dir: PathBuf },
    Rename { path: PathBuf },
}

impl PendingEdit {
    /// The directory whose listing the edit row sits in.
    pub fn dir(&self) -> &Path {
        match self {
            PendingEdit::NewFile { dir } | PendingEdit::NewFolder { dir } => dir,
            PendingEdit::Rename { path } => path.parent().unwrap_or(path),
        }
    }

    /// What the input starts out holding.
    pub fn initial_text(&self) -> String {
        match self {
            PendingEdit::Rename { path } => path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }
}

/// What one visible row is: an entry on disk, or the inline editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    Entry { is_dir: bool, expanded: bool },
    Editor(PendingEdit),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub kind: RowKind,
}

impl TreeRow {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, RowKind::Entry { is_dir: true, .. })
    }
}

/// The outcome of a file operation, for the panel to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileOp {
    /// A file now exists at this path (created or renamed) and should be
    /// opened / re-pointed in the panel.
    Created(PathBuf),
    /// `from` is now at `to`.
    Renamed { from: PathBuf, to: PathBuf },
    /// Gone from disk.
    Deleted(PathBuf),
}

/// Which selection gesture a click is (AGE-476 multi-select): a plain click
/// selects one entry, Ctrl/⌘ toggles the entry in the set, Shift extends
/// from the anchor over the visible rows in between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectGesture {
    Single,
    Toggle,
    Range,
}

pub struct FileTree {
    root: PathBuf,
    dirs: HashMap<PathBuf, DirListing>,
    expanded: HashSet<PathBuf>,
    /// The selected entries in the order they were selected; the last one
    /// is the primary (what `selected()` reports and new entries go next to).
    selection: Vec<PathBuf>,
    /// Where a Shift-range starts: the last plainly clicked or toggled row.
    anchor: Option<PathBuf>,
    pending: Option<PendingEdit>,
    last_sync: Option<Instant>,
    /// The last operation that failed, shown under the tree until the next
    /// one succeeds. Silent failures are the one thing an explorer must not
    /// have (`CLAUDE.md`, error handling).
    error: Option<String>,
}

impl FileTree {
    /// A tree rooted at `root` with the root listed and expanded.
    pub fn new(root: PathBuf) -> Self {
        let mut tree = Self {
            root: root.clone(),
            dirs: HashMap::new(),
            expanded: HashSet::new(),
            selection: Vec::new(),
            anchor: None,
            pending: None,
            last_sync: None,
            error: None,
        };
        tree.expanded.insert(root.clone());
        tree.dirs.insert(root.clone(), list_dir(&root));
        tree
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The primary selected entry (the most recently selected one).
    pub fn selected(&self) -> Option<&Path> {
        self.selection.last().map(PathBuf::as_path)
    }

    /// Every selected entry, oldest first.
    pub fn selection(&self) -> &[PathBuf] {
        &self.selection
    }

    pub fn is_selected(&self, path: &Path) -> bool {
        self.selection.iter().any(|p| p == path)
    }

    pub fn pending(&self) -> Option<&PendingEdit> {
        self.pending.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Surface a failure from outside the tree (a save that could not
    /// write) in the same slot its own operations use.
    pub fn set_error(&mut self, message: String) {
        self.error = Some(message);
    }

    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.expanded.contains(dir)
    }

    /// Make `path` the only selected entry (or clear the selection).
    pub fn select(&mut self, path: Option<PathBuf>) {
        self.selection.clear();
        if let Some(path) = path {
            self.anchor = Some(path.clone());
            self.selection.push(path);
        }
    }

    /// Apply a click on `path` with the given gesture.
    pub fn click_select(&mut self, path: PathBuf, gesture: SelectGesture) {
        match gesture {
            SelectGesture::Single => self.select(Some(path)),
            SelectGesture::Toggle => {
                if let Some(ix) = self.selection.iter().position(|p| p == &path) {
                    self.selection.remove(ix);
                } else {
                    self.selection.push(path.clone());
                }
                self.anchor = Some(path);
            }
            SelectGesture::Range => {
                let rows = self.rows();
                let anchor_ix = self
                    .anchor
                    .as_ref()
                    .and_then(|anchor| rows.iter().position(|row| &row.path == anchor));
                let target_ix = rows.iter().position(|row| row.path == path);
                match (anchor_ix, target_ix) {
                    (Some(a), Some(b)) => {
                        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                        // Keep what was selected outside the range, add the
                        // range in visible order; the anchor stays put so a
                        // second Shift-click re-extends from the same place.
                        let range: Vec<PathBuf> = rows[lo..=hi]
                            .iter()
                            .filter(|row| matches!(row.kind, RowKind::Entry { .. }))
                            .map(|row| row.path.clone())
                            .collect();
                        self.selection.retain(|p| !range.contains(p));
                        self.selection.extend(range);
                        // The clicked end is primary.
                        if let Some(ix) = self.selection.iter().position(|p| p == &path) {
                            let clicked = self.selection.remove(ix);
                            self.selection.push(clicked);
                        }
                    }
                    _ => self.select(Some(path)),
                }
            }
        }
    }

    /// Expand a directory (listing it on first sight) or collapse it.
    pub fn toggle(&mut self, dir: &Path) {
        if self.expanded.contains(dir) {
            self.expanded.remove(dir);
        } else {
            self.expand(dir);
        }
    }

    pub fn expand(&mut self, dir: &Path) {
        self.expanded.insert(dir.to_path_buf());
        if !self.dirs.contains_key(dir) {
            self.dirs.insert(dir.to_path_buf(), list_dir(dir));
        }
    }

    /// Expand every directory between the root and `path`, so a file the
    /// panel opened from elsewhere can be shown selected in the tree.
    pub fn reveal(&mut self, path: &Path) {
        if !path.starts_with(&self.root) {
            return;
        }
        let mut dir = self.root.clone();
        self.expand(&dir);
        if let Ok(rel) = path.strip_prefix(&self.root) {
            let mut components: Vec<_> = rel.components().collect();
            // The last component is the entry itself, not a directory to open.
            components.pop();
            for component in components {
                dir.push(component);
                self.expand(&dir);
            }
        }
        self.select(Some(path.to_path_buf()));
    }

    /// Re-list any expanded directory whose mtime moved since it was read.
    /// Rate-limited to [`SYNC_INTERVAL`]; `force` skips the limit (the
    /// refresh button, and after the tree's own operations).
    pub fn sync(&mut self, force: bool) -> bool {
        let now = Instant::now();
        if !force
            && self
                .last_sync
                .is_some_and(|last| now.duration_since(last) < SYNC_INTERVAL)
        {
            return false;
        }
        self.last_sync = Some(now);
        let mut changed = false;
        let expanded: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        for dir in expanded {
            let current = dir_mtime(&dir);
            let known = self.dirs.get(&dir).and_then(|listing| listing.mtime);
            if current.is_none() && dir != self.root {
                // Removed from under us: forget it rather than show a ghost.
                self.expanded.remove(&dir);
                self.dirs.remove(&dir);
                changed = true;
                continue;
            }
            if force || current != known {
                self.dirs.insert(dir.clone(), list_dir(&dir));
                changed = true;
            }
        }
        changed
    }

    /// The visible rows, root's children first, in listing order, with the
    /// inline editor (if any) in the slot it will occupy once committed.
    pub fn rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.push_rows(&self.root, 0, &mut rows);
        rows
    }

    fn push_rows(&self, dir: &Path, depth: usize, rows: &mut Vec<TreeRow>) {
        let Some(listing) = self.dirs.get(dir) else {
            return;
        };
        let pending_new =
            match &self.pending {
                Some(
                    edit @ (PendingEdit::NewFile { dir: d } | PendingEdit::NewFolder { dir: d }),
                ) if d == dir => Some(edit.clone()),
                _ => None,
            };
        if let Some(edit) = pending_new {
            rows.push(TreeRow {
                path: dir.to_path_buf(),
                name: String::new(),
                depth,
                kind: RowKind::Editor(edit),
            });
        }
        for node in &listing.entries {
            let renaming = matches!(
                &self.pending,
                Some(PendingEdit::Rename { path }) if path == &node.path
            );
            if renaming {
                rows.push(TreeRow {
                    path: node.path.clone(),
                    name: node.name.clone(),
                    depth,
                    kind: RowKind::Editor(PendingEdit::Rename {
                        path: node.path.clone(),
                    }),
                });
                continue;
            }
            let expanded = node.is_dir && self.expanded.contains(&node.path);
            rows.push(TreeRow {
                path: node.path.clone(),
                name: node.name.clone(),
                depth,
                kind: RowKind::Entry {
                    is_dir: node.is_dir,
                    expanded,
                },
            });
            if expanded {
                self.push_rows(&node.path, depth + 1, rows);
            }
        }
    }

    /// The directory a "new entry" from the current selection lands in: the
    /// selected directory, the selected file's parent, or the root.
    pub fn target_dir(&self) -> PathBuf {
        match self.selection.last() {
            Some(path) if path.is_dir() => path.clone(),
            Some(path) => path
                .parent()
                .filter(|parent| parent.starts_with(&self.root))
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        }
    }

    /// Start an inline edit; the directory it sits in is expanded so the
    /// row is visible.
    pub fn begin_edit(&mut self, edit: PendingEdit) {
        let dir = edit.dir().to_path_buf();
        self.expand(&dir);
        self.error = None;
        self.pending = Some(edit);
    }

    pub fn cancel_edit(&mut self) {
        self.pending = None;
    }

    /// Commit the pending edit with the typed `name`. On success the
    /// affected directory is re-listed and the outcome returned; on failure
    /// the edit stays open with the reason in [`Self::error`].
    pub fn commit_edit(&mut self, name: &str) -> Option<FileOp> {
        let edit = self.pending.clone()?;
        let result = match &edit {
            PendingEdit::NewFile { dir } => create_entry(dir, name, false).map(FileOp::Created),
            PendingEdit::NewFolder { dir } => create_entry(dir, name, true).map(FileOp::Created),
            PendingEdit::Rename { path } => rename_entry(path, name).map(|to| FileOp::Renamed {
                from: path.clone(),
                to,
            }),
        };
        match result {
            Ok(op) => {
                self.pending = None;
                self.error = None;
                self.after_change(&op);
                Some(op)
            }
            Err(message) => {
                self.error = Some(message);
                None
            }
        }
    }

    /// Delete `path` (a file, or a directory and everything in it).
    pub fn delete(&mut self, path: &Path) -> Option<FileOp> {
        match delete_entry(path) {
            Ok(()) => {
                self.error = None;
                let op = FileOp::Deleted(path.to_path_buf());
                self.after_change(&op);
                Some(op)
            }
            Err(message) => {
                self.error = Some(message);
                None
            }
        }
    }

    /// Delete several entries (the multi-selection). Each failure is
    /// reported once at the end; what could be deleted is.
    pub fn delete_many(&mut self, paths: &[PathBuf]) -> Vec<FileOp> {
        let mut ops = Vec::new();
        let mut failures = Vec::new();
        // Skip anything inside another entry being deleted: it goes with
        // its parent, and deleting it first would just fail the parent's
        // listing refresh for nothing.
        for path in paths {
            if paths
                .iter()
                .any(|other| other != path && path.starts_with(other))
            {
                continue;
            }
            match delete_entry(path) {
                Ok(()) => {
                    let op = FileOp::Deleted(path.clone());
                    self.after_change(&op);
                    ops.push(op);
                }
                Err(message) => failures.push(message),
            }
        }
        self.error = (!failures.is_empty()).then(|| failures.join(" "));
        ops
    }

    /// Move entries into `dest` (drag-and-drop). An entry already in
    /// `dest`, or a folder dropped into itself or one of its descendants,
    /// is left alone; a name clash refuses that entry. Returns one
    /// `Renamed` per entry that moved.
    pub fn move_entries(&mut self, paths: &[PathBuf], dest: &Path) -> Vec<FileOp> {
        let mut ops = Vec::new();
        let mut failures = Vec::new();
        for path in paths {
            if path.parent() == Some(dest) || dest.starts_with(path) {
                continue;
            }
            match move_entry(path, dest) {
                Ok(to) => {
                    let op = FileOp::Renamed {
                        from: path.clone(),
                        to,
                    };
                    self.after_change(&op);
                    ops.push(op);
                }
                Err(message) => failures.push(message),
            }
        }
        if !ops.is_empty() {
            self.expand(dest);
            // The moved entries are the selection now, in their new place.
            self.selection = ops
                .iter()
                .filter_map(|op| match op {
                    FileOp::Renamed { to, .. } => Some(to.clone()),
                    _ => None,
                })
                .collect();
            self.anchor = self.selection.last().cloned();
        }
        self.error = (!failures.is_empty()).then(|| failures.join(" "));
        ops
    }

    fn after_change(&mut self, op: &FileOp) {
        match op {
            FileOp::Created(path) => self.select(Some(path.clone())),
            FileOp::Renamed { from, to } => {
                // A renamed directory takes its expanded state with it.
                if self.expanded.remove(from) {
                    self.expanded.insert(to.clone());
                }
                self.dirs.remove(from);
                self.select(Some(to.clone()));
            }
            FileOp::Deleted(path) => {
                self.expanded.retain(|dir| !dir.starts_with(path));
                self.dirs.retain(|dir, _| !dir.starts_with(path));
                self.selection.retain(|s| !s.starts_with(path));
                if self.anchor.as_ref().is_some_and(|a| a.starts_with(path)) {
                    self.anchor = None;
                }
            }
        }
        self.sync(true);
    }
}

/// A directory's entries, folders first, then case-insensitively by name,
/// with [`HIDDEN_NAMES`] left out. An unreadable directory lists as empty
/// rather than failing the whole tree.
fn list_dir(dir: &Path) -> DirListing {
    let mtime = dir_mtime(dir);
    let Ok(read) = std::fs::read_dir(dir) else {
        return DirListing {
            entries: Vec::new(),
            mtime,
        };
    };
    let mut entries: Vec<TreeNode> = read
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if HIDDEN_NAMES.contains(&name.as_str()) {
                return None;
            }
            // `metadata` follows symlinks, so a link to a directory expands
            // like one; a dangling link is listed as a file.
            let is_dir = entry
                .path()
                .metadata()
                .map(|meta| meta.is_dir())
                .unwrap_or(false);
            Some(TreeNode {
                path: entry.path(),
                name,
                is_dir,
            })
        })
        .collect();
    sort_nodes(&mut entries);
    DirListing { entries, mtime }
}

fn sort_nodes(nodes: &mut [TreeNode]) {
    nodes.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

fn dir_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).ok()?.modified().ok()
}

/// Where `path` is after `from` moved to `to`: `to` itself for `from`, the
/// same relative tail under `to` for anything inside it, `None` otherwise.
/// (`to.join("")` would leave a trailing separator, which is a different
/// path to `std::fs`.)
pub fn rebase_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_path_buf()
    } else {
        to.join(rest)
    })
}

/// A single path component the user typed: not empty, not `.`/`..`, and
/// not a path — the tree creates entries in one directory at a time.
pub fn validate_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name.".to_string());
    }
    if name == "." || name == ".." {
        return Err(format!("'{name}' is not a valid name."));
    }
    if name.contains('/') || name.contains('\\') {
        return Err("A name cannot contain a path separator.".to_string());
    }
    Ok(name)
}

fn create_entry(dir: &Path, name: &str, is_dir: bool) -> Result<PathBuf, String> {
    let name = validate_name(name)?;
    let target = dir.join(name);
    if target.exists() {
        return Err(format!("'{name}' already exists."));
    }
    let result = if is_dir {
        std::fs::create_dir(&target)
    } else {
        std::fs::write(&target, "")
    };
    result.map_err(|e| format!("Could not create '{name}': {e}"))?;
    Ok(target)
}

fn rename_entry(path: &Path, name: &str) -> Result<PathBuf, String> {
    let name = validate_name(name)?;
    let parent = path
        .parent()
        .ok_or_else(|| "Cannot rename the root.".to_string())?;
    let target = parent.join(name);
    if target == path {
        return Ok(target);
    }
    if target.exists() {
        return Err(format!("'{name}' already exists."));
    }
    std::fs::rename(path, &target).map_err(|e| format!("Could not rename: {e}"))?;
    Ok(target)
}

fn move_entry(path: &Path, dest: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| "Cannot move the root.".to_string())?;
    let target = dest.join(name);
    if target.exists() {
        return Err(format!(
            "'{}' already exists in '{}'.",
            name.to_string_lossy(),
            dest.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| dest.display().to_string())
        ));
    }
    std::fs::rename(path, &target)
        .map_err(|e| format!("Could not move '{}': {e}", name.to_string_lossy()))?;
    Ok(target)
}

fn delete_entry(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("Could not delete: {e}"))?;
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| format!("Could not delete: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(rows: &[TreeRow]) -> Vec<(String, usize)> {
        rows.iter()
            .map(|row| (row.name.clone(), row.depth))
            .collect()
    }

    fn workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("README.md"), "# hi\n").unwrap();
        fs::write(root.join("Cargo.toml"), "").unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("src/nested/deep.rs"), "").unwrap();
        fs::write(root.join(".env"), "").unwrap();
        tmp
    }

    #[test]
    fn root_lists_folders_first_then_files_case_insensitively_and_hides_git() {
        let tmp = workspace();
        let tree = FileTree::new(tmp.path().to_path_buf());
        assert_eq!(
            names(&tree.rows()),
            vec![
                ("docs".to_string(), 0),
                ("src".to_string(), 0),
                (".env".to_string(), 0),
                ("Cargo.toml".to_string(), 0),
                ("README.md".to_string(), 0),
            ]
        );
    }

    #[test]
    fn expanding_a_folder_lists_it_lazily_and_collapsing_hides_it_again() {
        let tmp = workspace();
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        let src = tmp.path().join("src");
        tree.toggle(&src);
        assert_eq!(
            names(&tree.rows()),
            vec![
                ("docs".to_string(), 0),
                ("src".to_string(), 0),
                ("nested".to_string(), 1),
                ("main.rs".to_string(), 1),
                (".env".to_string(), 0),
                ("Cargo.toml".to_string(), 0),
                ("README.md".to_string(), 0),
            ]
        );
        assert!(tree.is_expanded(&src));
        tree.toggle(&src);
        assert!(!tree.is_expanded(&src));
        assert_eq!(tree.rows().len(), 5);
    }

    #[test]
    fn an_empty_folder_is_still_a_folder() {
        let tmp = workspace();
        let tree = FileTree::new(tmp.path().to_path_buf());
        let docs = tree
            .rows()
            .into_iter()
            .find(|row| row.name == "docs")
            .unwrap();
        assert!(docs.is_dir());
    }

    #[test]
    fn reveal_expands_the_way_down_and_selects() {
        let tmp = workspace();
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        let deep = tmp.path().join("src/nested/deep.rs");
        tree.reveal(&deep);
        assert!(tree.is_expanded(&tmp.path().join("src")));
        assert!(tree.is_expanded(&tmp.path().join("src/nested")));
        assert_eq!(tree.selected(), Some(deep.as_path()));
        assert!(tree.rows().iter().any(|row| row.path == deep));
    }

    #[test]
    fn reveal_outside_the_root_is_ignored() {
        let tmp = workspace();
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        tree.reveal(Path::new("/definitely/elsewhere/x.rs"));
        assert_eq!(tree.selected(), None);
        assert_eq!(tree.rows().len(), 5);
    }

    #[test]
    fn a_forced_sync_picks_up_files_written_behind_its_back() {
        let tmp = workspace();
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        fs::write(tmp.path().join("new.txt"), "x").unwrap();
        assert!(!tree.rows().iter().any(|row| row.name == "new.txt"));
        assert!(tree.sync(true));
        assert!(tree.rows().iter().any(|row| row.name == "new.txt"));
    }

    #[test]
    fn an_unforced_sync_is_rate_limited() {
        let tmp = workspace();
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        tree.sync(false); // stamps the clock
        fs::write(tmp.path().join("late.txt"), "x").unwrap();
        // Immediately again: inside the interval, nothing is re-read.
        assert!(!tree.sync(false));
        assert!(!tree.rows().iter().any(|row| row.name == "late.txt"));
    }

    #[test]
    fn an_expanded_folder_that_vanishes_is_forgotten() {
        let tmp = workspace();
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        let docs = tmp.path().join("docs");
        tree.expand(&docs);
        fs::remove_dir(&docs).unwrap();
        tree.sync(true);
        assert!(!tree.is_expanded(&docs));
        assert!(!tree.rows().iter().any(|row| row.name == "docs"));
    }

    #[test]
    fn target_dir_follows_the_selection() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        assert_eq!(tree.target_dir(), root);
        tree.select(Some(root.join("src/main.rs")));
        assert_eq!(tree.target_dir(), root.join("src"));
        tree.select(Some(root.join("src")));
        assert_eq!(tree.target_dir(), root.join("src"));
    }

    #[test]
    fn a_new_file_edit_row_sits_first_in_its_folder_and_commits_to_disk() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        tree.begin_edit(PendingEdit::NewFile {
            dir: root.join("src"),
        });
        let rows = tree.rows();
        let src_ix = rows.iter().position(|row| row.name == "src").unwrap();
        assert!(matches!(
            rows[src_ix + 1].kind,
            RowKind::Editor(PendingEdit::NewFile { .. })
        ));
        assert_eq!(rows[src_ix + 1].depth, 1);

        let op = tree.commit_edit("lib.rs");
        assert_eq!(op, Some(FileOp::Created(root.join("src/lib.rs"))));
        assert!(root.join("src/lib.rs").is_file());
        assert!(tree.pending().is_none());
        assert_eq!(tree.selected(), Some(root.join("src/lib.rs").as_path()));
        assert!(tree.rows().iter().any(|row| row.name == "lib.rs"));
    }

    #[test]
    fn a_new_folder_commits_as_a_directory() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        tree.begin_edit(PendingEdit::NewFolder { dir: root.clone() });
        assert_eq!(
            tree.commit_edit("notes"),
            Some(FileOp::Created(root.join("notes")))
        );
        assert!(root.join("notes").is_dir());
    }

    #[test]
    fn a_bad_name_keeps_the_edit_open_with_a_reason() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        tree.begin_edit(PendingEdit::NewFile { dir: root.clone() });
        assert_eq!(tree.commit_edit(""), None);
        assert_eq!(tree.error(), Some("Enter a name."));
        assert_eq!(tree.commit_edit("a/b"), None);
        assert!(tree.error().unwrap().contains("separator"));
        assert_eq!(tree.commit_edit("README.md"), None);
        assert!(tree.error().unwrap().contains("already exists"));
        assert!(tree.pending().is_some());
        tree.cancel_edit();
        assert!(tree.pending().is_none());
    }

    #[test]
    fn a_rename_row_replaces_the_entry_and_moves_it_on_disk() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        let readme = root.join("README.md");
        tree.begin_edit(PendingEdit::Rename {
            path: readme.clone(),
        });
        assert_eq!(tree.pending().unwrap().initial_text(), "README.md");
        let rows = tree.rows();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().any(|row| matches!(
            &row.kind,
            RowKind::Editor(PendingEdit::Rename { path }) if path == &readme
        )));
        let op = tree.commit_edit("GUIDE.md");
        assert_eq!(
            op,
            Some(FileOp::Renamed {
                from: readme.clone(),
                to: root.join("GUIDE.md"),
            })
        );
        assert!(!readme.exists());
        assert_eq!(fs::read_to_string(root.join("GUIDE.md")).unwrap(), "# hi\n");
    }

    #[test]
    fn renaming_an_expanded_folder_keeps_it_expanded() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        let src = root.join("src");
        tree.expand(&src);
        tree.begin_edit(PendingEdit::Rename { path: src.clone() });
        tree.commit_edit("lib");
        assert!(tree.is_expanded(&root.join("lib")));
        assert!(!tree.is_expanded(&src));
        assert!(tree.rows().iter().any(|row| row.name == "main.rs"));
    }

    #[test]
    fn delete_removes_files_and_whole_folders() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        let src = root.join("src");
        tree.expand(&src);
        tree.select(Some(src.join("main.rs")));
        assert_eq!(
            tree.delete(&root.join("Cargo.toml")),
            Some(FileOp::Deleted(root.join("Cargo.toml")))
        );
        assert!(!root.join("Cargo.toml").exists());
        assert_eq!(tree.delete(&src), Some(FileOp::Deleted(src.clone())));
        assert!(!src.exists());
        assert!(!tree.is_expanded(&src));
        assert_eq!(tree.selected(), None);
        assert_eq!(
            names(&tree.rows()),
            vec![
                ("docs".to_string(), 0),
                (".env".to_string(), 0),
                ("README.md".to_string(), 0),
            ]
        );
    }

    #[test]
    fn deleting_something_already_gone_reports_instead_of_panicking() {
        let tmp = workspace();
        let mut tree = FileTree::new(tmp.path().to_path_buf());
        assert_eq!(tree.delete(&tmp.path().join("nope")), None);
        assert!(tree.error().unwrap().starts_with("Could not delete"));
    }

    #[test]
    fn toggle_builds_a_set_and_the_last_one_is_primary() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        tree.click_select(root.join("Cargo.toml"), SelectGesture::Single);
        tree.click_select(root.join("README.md"), SelectGesture::Toggle);
        assert_eq!(
            tree.selection(),
            &[root.join("Cargo.toml"), root.join("README.md")]
        );
        assert_eq!(tree.selected(), Some(root.join("README.md").as_path()));
        // Toggling again removes it.
        tree.click_select(root.join("README.md"), SelectGesture::Toggle);
        assert_eq!(tree.selection(), &[root.join("Cargo.toml")]);
        // A plain click collapses the set to one.
        tree.click_select(root.join(".env"), SelectGesture::Single);
        assert_eq!(tree.selection(), &[root.join(".env")]);
    }

    #[test]
    fn shift_selects_the_visible_range_from_the_anchor() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        tree.expand(&root.join("src"));
        // Rows: docs, src, nested, main.rs, .env, Cargo.toml, README.md
        tree.click_select(root.join("src"), SelectGesture::Single);
        tree.click_select(root.join(".env"), SelectGesture::Range);
        assert_eq!(
            tree.selection(),
            &[
                root.join("src"),
                root.join("src/nested"),
                root.join("src/main.rs"),
                root.join(".env"),
            ]
        );
        assert!(tree.is_selected(&root.join("src/main.rs")));
        // Re-extending from the same anchor upwards replaces the range.
        tree.click_select(root.join("docs"), SelectGesture::Range);
        assert!(tree.is_selected(&root.join("docs")));
        assert!(tree.is_selected(&root.join("src")));
        assert!(
            tree.is_selected(&root.join(".env")),
            "earlier range is kept"
        );
    }

    #[test]
    fn shift_without_an_anchor_is_a_plain_click() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        tree.click_select(root.join("README.md"), SelectGesture::Range);
        assert_eq!(tree.selection(), &[root.join("README.md")]);
    }

    #[test]
    fn delete_many_skips_children_of_a_deleted_folder_and_reports_failures() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        let ops = tree.delete_many(&[
            root.join("src/main.rs"),
            root.join("src"),
            root.join("Cargo.toml"),
            root.join("missing"),
        ]);
        assert_eq!(
            ops,
            vec![
                FileOp::Deleted(root.join("src")),
                FileOp::Deleted(root.join("Cargo.toml")),
            ]
        );
        assert!(!root.join("src").exists());
        assert!(!root.join("Cargo.toml").exists());
        assert!(tree.error().unwrap().contains("Could not delete"));
        assert!(tree.selection().is_empty());
    }

    #[test]
    fn move_entries_moves_into_the_folder_and_selects_them_there() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        let ops = tree.move_entries(
            &[root.join("Cargo.toml"), root.join("README.md")],
            &root.join("docs"),
        );
        assert_eq!(
            ops,
            vec![
                FileOp::Renamed {
                    from: root.join("Cargo.toml"),
                    to: root.join("docs/Cargo.toml"),
                },
                FileOp::Renamed {
                    from: root.join("README.md"),
                    to: root.join("docs/README.md"),
                },
            ]
        );
        assert_eq!(
            fs::read_to_string(root.join("docs/README.md")).unwrap(),
            "# hi\n"
        );
        assert!(!root.join("README.md").exists());
        assert!(tree.is_expanded(&root.join("docs")));
        assert_eq!(
            tree.selection(),
            &[root.join("docs/Cargo.toml"), root.join("docs/README.md")]
        );
        assert!(tree.error().is_none());
    }

    #[test]
    fn move_entries_refuses_a_folder_into_itself_and_a_name_clash() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        fs::write(root.join("docs/README.md"), "other").unwrap();
        let mut tree = FileTree::new(root.clone());
        // Into itself / a descendant: silently left alone.
        assert!(
            tree.move_entries(&[root.join("src")], &root.join("src/nested"))
                .is_empty()
        );
        assert!(root.join("src/nested/deep.rs").exists());
        // Already there: left alone, no error.
        assert!(
            tree.move_entries(&[root.join("Cargo.toml")], &root)
                .is_empty()
        );
        assert!(tree.error().is_none());
        // Name clash: refused with a reason, the file stays.
        assert!(
            tree.move_entries(&[root.join("README.md")], &root.join("docs"))
                .is_empty()
        );
        assert!(tree.error().unwrap().contains("already exists"));
        assert_eq!(
            fs::read_to_string(root.join("README.md")).unwrap(),
            "# hi\n"
        );
    }

    #[test]
    fn moving_an_expanded_folder_keeps_it_expanded_at_its_new_place() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut tree = FileTree::new(root.clone());
        tree.expand(&root.join("src"));
        tree.move_entries(&[root.join("src")], &root.join("docs"));
        assert!(tree.is_expanded(&root.join("docs/src")));
        assert!(
            tree.rows()
                .iter()
                .any(|row| row.path == root.join("docs/src/main.rs"))
        );
    }

    #[test]
    fn rebase_path_maps_the_entry_itself_and_its_descendants_only() {
        let from = Path::new("/ws/docs");
        let to = Path::new("/ws/notes");
        assert_eq!(
            rebase_path(Path::new("/ws/docs"), from, to),
            Some(PathBuf::from("/ws/notes"))
        );
        assert_eq!(
            rebase_path(Path::new("/ws/docs/a/b.md"), from, to),
            Some(PathBuf::from("/ws/notes/a/b.md"))
        );
        assert_eq!(rebase_path(Path::new("/ws/docs2/x"), from, to), None);
        assert_eq!(rebase_path(Path::new("/ws/README.md"), from, to), None);
        // A renamed file: exactly `to`, no trailing separator.
        let got = rebase_path(
            Path::new("/ws/a.txt"),
            Path::new("/ws/a.txt"),
            Path::new("/ws/b.txt"),
        )
        .unwrap();
        assert_eq!(got.as_os_str(), "/ws/b.txt");
    }

    #[test]
    fn walk_files_recurses_and_hides_git() {
        let tmp = workspace();
        let root = tmp.path().to_path_buf();
        let mut files: Vec<String> = walk_files(&root)
            .into_iter()
            .map(|p| p.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![
                ".env".to_string(),
                "Cargo.toml".to_string(),
                "README.md".to_string(),
                "src/main.rs".to_string(),
                "src/nested/deep.rs".to_string(),
            ]
        );
    }

    #[test]
    fn validate_name_rejects_dots_and_separators() {
        assert!(validate_name(".").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a\\b").is_err());
        assert_eq!(validate_name("  ok.txt "), Ok("ok.txt"));
    }
}
