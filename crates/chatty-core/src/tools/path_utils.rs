//! Output paths for tools that render a file into the workspace
//! (`create_chart`'s `save_path`, `compile_typst`'s `output_path`).
//!
//! With the execution settings' `output_root` set (AGE-1747), a relative
//! output path of these tools and of the document writers (`write_docx`,
//! `write_excel`, `edit_excel`'s `output_path`, `write_pptx`) is placed
//! under it ([`under_output_root`]); an absolute path inside the workspace
//! is kept as given. Unset, every path is taken as it always was.
//!
//! Both write only inside the workspace (AGE-534): no `~` escape hatch, no
//! absolute path outside the workspace root, no write through a symlink, and
//! the write is asked about (`request_write_approval`) like every other
//! file write.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use crate::models::write_approval_store::{PendingWriteApprovals, WriteOperation};
use crate::services::no_follow_write::{entry_exists, write_no_follow};
use crate::settings::models::execution_settings::ApprovalMode;
use crate::tools::filesystem_write_tool::request_write_approval;

/// Normalize a path by resolving `.` and `..` components without filesystem access.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                components.pop();
            }
            Component::CurDir => {}
            c => components.push(c),
        }
    }
    components.iter().collect()
}

/// `path` with its deepest existing ancestor canonicalized (symlinks
/// resolved) and the not-yet-existing rest appended. `path` must be
/// normalized, so the rest holds plain names only.
fn anchor_to_disk(path: &Path) -> Result<PathBuf, String> {
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    let mut existing = path;
    loop {
        if std::fs::symlink_metadata(existing).is_ok() {
            let canonical = std::fs::canonicalize(existing).map_err(|e| {
                format!(
                    "Could not resolve '{}' (a broken symlink?): {e}",
                    existing.display()
                )
            })?;
            return Ok(rest
                .iter()
                .rev()
                .fold(canonical, |acc, name| acc.join(name)));
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return Err(format!("No existing ancestor for '{}'", path.display())),
        }
    }
}

/// `path` placed under `output_root` (AGE-1747), before it is resolved:
/// a relative path is joined onto `output_root`; an absolute path, a `~`
/// path (which the resolvers refuse), an empty path or no `output_root`
/// leaves it as given. Only a path the model wrote goes through here; the
/// workspace bounds check that follows is unchanged.
pub(super) fn under_output_root<'a>(output_root: Option<&Path>, path: &'a str) -> Cow<'a, str> {
    match output_root {
        Some(output_root)
            if !path.trim().is_empty()
                && !path.starts_with('~')
                && !Path::new(path).is_absolute() =>
        {
            Cow::Owned(output_root.join(path).to_string_lossy().into_owned())
        }
        _ => Cow::Borrowed(path),
    }
}

/// One sentence for a writing tool's description saying where a relative
/// `param` goes, when `output_root` is set (AGE-1747); empty otherwise, so
/// a description without one is the text it always was.
pub(super) fn output_root_note(output_root: Option<&Path>, param: &str) -> String {
    output_root.map_or_else(String::new, |root| {
        format!(
            "\n\nA relative {param} is saved under `{}/` in the workspace; \
             an absolute path inside the workspace is kept as given.",
            root.display()
        )
    })
}

/// Resolve a tool's output file inside the workspace `root` (canonical, as
/// [`FileSystemService::workspace_root`] returns it).
///
/// - `~` and `~/…` are refused: these tools write into the workspace only.
/// - Relative paths are taken against `root`; absolute paths must land in
///   it, except that Harbor / AppWorld's `/app/…` maps onto `root`.
/// - Symlinks in the existing part of the path are resolved before the
///   bounds check, so a link out of the workspace is refused, and an
///   existing target must be a regular file (not a symlink or directory).
///
/// [`FileSystemService::workspace_root`]: crate::services::filesystem_service::FileSystemService::workspace_root
pub(super) fn resolve_workspace_output(root: &Path, path: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("Output path cannot be empty".to_string());
    }
    if path == "~" || path.starts_with("~/") {
        return Err(format!(
            "Output path '{path}' is home-relative; '~' is not allowed. \
             Give a path inside the workspace (e.g. relative to the workspace root)"
        ));
    }

    let p = Path::new(path);
    let mut candidates = vec![normalize_path(&if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    })];
    // Harbor / AppWorld agents often emit `/app/...`. Map that onto the
    // workspace instead of rejecting it or writing to a host `/app`.
    if p.is_absolute()
        && let Ok(rest) = candidates[0].strip_prefix("/app")
    {
        candidates.push(normalize_path(&root.join(rest)));
    }

    for candidate in candidates {
        let resolved = anchor_to_disk(&candidate)?;
        if !resolved.starts_with(root) || resolved == root {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(&resolved)
            && !meta.file_type().is_file()
        {
            return Err(format!(
                "Output path '{path}' exists and is not a regular file; refusing to write over it"
            ));
        }
        return Ok(resolved);
    }
    Err(format!(
        "Output path '{path}' is outside the workspace directory"
    ))
}

/// Ask for approval to write `path` under `approval`'s mode; `None` (no
/// approval wired, i.e. not an agent's tool) lets the write through.
/// Returns whether the write may go ahead.
pub(super) async fn approve_output_write(
    approval: Option<&(ApprovalMode, PendingWriteApprovals)>,
    path: &Path,
    content_preview: String,
) -> Result<bool, String> {
    let Some((mode, pending)) = approval else {
        return Ok(true);
    };
    request_write_approval(
        pending,
        mode,
        WriteOperation::WriteFile {
            path: path.display().to_string(),
            is_overwrite: entry_exists(path),
            content_preview,
        },
    )
    .await
    .map_err(|e| e.to_string())
}

/// Write `bytes` to `path` (from [`resolve_workspace_output`]) without
/// following a symlink: a new file is created with `create_new`, and an
/// existing regular file is replaced by renaming a fresh sibling over it,
/// which swaps the directory entry rather than writing through it
/// (AGE-1097's approach; shared with the other writers in
/// [`no_follow_write`], AGE-1113). Parent directories are created, then
/// checked to still resolve inside `root`.
///
/// [`no_follow_write`]: crate::services::no_follow_write
pub(super) fn write_output_file(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Output path '{}' has no parent", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("Could not create directory '{}': {e}", parent.display()))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("Could not resolve '{}': {e}", parent.display()))?;
    if !canonical_parent.starts_with(root) {
        return Err(format!(
            "Output path '{}' is outside the workspace directory",
            path.display()
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| format!("Output path '{}' has no file name", path.display()))?;
    let target = canonical_parent.join(name);

    write_no_follow(&target, bytes)
        .map(|_| ())
        .map_err(|e| format!("Failed to write '{}': {e}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        (tmp, root)
    }

    #[test]
    fn relative_and_nested_paths_resolve_inside_the_workspace() {
        let (_tmp, root) = workspace();
        assert_eq!(
            resolve_workspace_output(&root, "report.pdf").unwrap(),
            root.join("report.pdf")
        );
        assert_eq!(
            resolve_workspace_output(&root, "sub/dir/a/../out.pdf").unwrap(),
            root.join("sub/dir/out.pdf")
        );
    }

    #[test]
    fn absolute_path_inside_workspace_is_kept() {
        let (_tmp, root) = workspace();
        let inside = root.join("charts/x.png");
        assert_eq!(
            resolve_workspace_output(&root, inside.to_str().unwrap()).unwrap(),
            inside
        );
    }

    #[test]
    fn absolute_path_outside_workspace_is_refused() {
        let (_tmp, root) = workspace();
        let err = resolve_workspace_output(&root, "/etc/report.pdf").unwrap_err();
        assert!(err.contains("outside the workspace directory"), "{err}");
    }

    #[test]
    fn traversal_out_of_the_workspace_is_refused() {
        let (_tmp, root) = workspace();
        for path in ["../../etc/passwd", "a/../../x.pdf", ".."] {
            let err = resolve_workspace_output(&root, path).unwrap_err();
            assert!(
                err.contains("outside the workspace directory"),
                "{path}: {err}"
            );
        }
    }

    #[test]
    fn harbor_app_prefix_remaps_into_workspace() {
        let (_tmp, root) = workspace();
        assert_eq!(
            resolve_workspace_output(&root, "/app/charts/sales.png").unwrap(),
            root.join("charts/sales.png")
        );
    }

    #[test]
    fn tilde_alone_is_refused_not_a_panic() {
        let (_tmp, root) = workspace();
        let err = resolve_workspace_output(&root, "~").unwrap_err();
        assert!(err.contains("'~' is not allowed"), "{err}");
    }

    #[test]
    fn tilde_paths_are_refused() {
        let (_tmp, root) = workspace();
        for path in ["~/../../x", "~/documents/report.pdf"] {
            let err = resolve_workspace_output(&root, path).unwrap_err();
            assert!(err.contains("'~' is not allowed"), "{path}: {err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_out_of_the_workspace_is_refused() {
        let (_tmp, root) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
        let err = resolve_workspace_output(&root, "escape/x.png").unwrap_err();
        assert!(err.contains("outside the workspace directory"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_target_file_is_refused() {
        let (_tmp, root) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        // Pointing-out and dangling links are both refused.
        std::os::unix::fs::symlink(&victim, root.join("out.png")).unwrap();
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling.png")).unwrap();
        assert!(resolve_workspace_output(&root, "out.png").is_err());
        assert!(resolve_workspace_output(&root, "dangling.png").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[cfg(unix)]
    #[test]
    fn write_never_follows_a_symlink_planted_after_resolving() {
        let (_tmp, root) = workspace();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        let target = resolve_workspace_output(&root, "out.png").unwrap();
        std::os::unix::fs::symlink(&victim, &target).unwrap();
        assert!(write_output_file(&root, &target, b"png").is_err());
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }

    /// AGE-1747: with an output root, a relative output path lands under
    /// it; an absolute path inside the workspace is still allowed as given;
    /// without one, nothing moves.
    #[test]
    fn a_relative_output_path_lands_under_the_output_root() {
        let (_tmp, root) = workspace();
        let outputs = Some(Path::new("outputs"));
        assert_eq!(
            resolve_workspace_output(&root, &under_output_root(outputs, "report.pdf")).unwrap(),
            root.join("outputs/report.pdf")
        );
        assert_eq!(
            resolve_workspace_output(&root, &under_output_root(outputs, "charts/a.png")).unwrap(),
            root.join("outputs/charts/a.png")
        );
        let inside = root.join("elsewhere/x.pdf");
        let inside = inside.to_str().unwrap();
        assert_eq!(under_output_root(outputs, inside), inside);
        assert_eq!(
            resolve_workspace_output(&root, &under_output_root(outputs, inside)).unwrap(),
            root.join("elsewhere/x.pdf")
        );
        assert_eq!(under_output_root(None, "report.pdf"), "report.pdf");
        for refused in ["~", "~/x.pdf", ""] {
            assert_eq!(under_output_root(outputs, refused), refused);
        }
    }

    /// The output root moves a path, never past the workspace bounds.
    #[test]
    fn the_output_root_does_not_widen_the_workspace() {
        let (_tmp, root) = workspace();
        let outputs = Some(Path::new("outputs"));
        let err = resolve_workspace_output(&root, &under_output_root(outputs, "../../x.pdf"))
            .unwrap_err();
        assert!(err.contains("outside the workspace directory"), "{err}");
        let err = resolve_workspace_output(&root, &under_output_root(Some(Path::new("/etc")), "x"))
            .unwrap_err();
        assert!(err.contains("outside the workspace directory"), "{err}");
    }

    #[test]
    fn the_output_root_note_is_empty_without_one() {
        assert_eq!(output_root_note(None, "path"), "");
        assert!(output_root_note(Some(Path::new("outputs")), "path").contains("`outputs/`"));
    }

    #[test]
    fn write_creates_parents_and_overwrites_a_regular_file() {
        let (_tmp, root) = workspace();
        let target = resolve_workspace_output(&root, "a/b/out.pdf").unwrap();
        write_output_file(&root, &target, b"one").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"one");
        write_output_file(&root, &target, b"two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(root.join("a/b")).unwrap().count(), 1);
    }
}
