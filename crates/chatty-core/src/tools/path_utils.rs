use std::path::{Path, PathBuf};

/// Normalize a path by resolving `.` and `..` components without filesystem access.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                components.pop();
            }
            std::path::Component::CurDir => {}
            c => components.push(c),
        }
    }
    components.iter().collect()
}

/// Resolve an output file path for tool use, expanding `~` and applying workspace
/// bounds-checking.
///
/// Path resolution rules:
/// - `~` paths expand to the home directory and are **not** workspace-bounded (the
///   user explicitly requested a path outside the workspace root).
/// - Absolute paths are normalized. When `workspace_dir` is configured they **must**
///   remain within the workspace.
/// - Relative paths are resolved against `workspace_dir` (falling back to the home
///   directory when no workspace is configured) and bounds-checked against the workspace.
///
/// Returns the resolved absolute [`PathBuf`], or an error string if the path would
/// escape the configured workspace.
pub(super) fn resolve_output_path(
    path: &str,
    workspace_dir: Option<&str>,
) -> Result<PathBuf, String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

    // Home-relative: expand `~` and allow unrestricted (explicit user choice).
    if path.starts_with("~/") || path == "~" {
        return Ok(normalize_path(&home.join(&path[2..])));
    }

    let p = Path::new(path);

    if p.is_absolute() {
        let normalized = normalize_path(p);
        // When a workspace is configured, absolute paths must stay within it.
        if let Some(workspace) = workspace_dir {
            let workspace_norm = normalize_path(Path::new(workspace));
            if !normalized.starts_with(&workspace_norm) {
                return Err(format!(
                    "Output path '{}' is outside the workspace directory",
                    path
                ));
            }
        }
        return Ok(normalized);
    }

    // Relative path: resolve against workspace_dir (or home as fallback).
    let base = workspace_dir
        .map(PathBuf::from)
        .unwrap_or_else(|| home.clone());
    let resolved = normalize_path(&base.join(p));

    // Bounds-check: ensure the resolved path stays within the workspace.
    if let Some(workspace) = workspace_dir {
        let workspace_norm = normalize_path(Path::new(workspace));
        if !resolved.starts_with(&workspace_norm) {
            return Err(format!(
                "Output path '{}' resolves outside the workspace directory",
                path
            ));
        }
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_absolute_path_no_workspace() {
        let resolved = resolve_output_path("/tmp/report.pdf", None).unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/report.pdf"));
    }

    #[test]
    fn test_absolute_path_inside_workspace() {
        let resolved = resolve_output_path("/workspace/report.pdf", Some("/workspace")).unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/report.pdf"));
    }

    #[test]
    fn test_absolute_path_outside_workspace_rejected() {
        let result = resolve_output_path("/tmp/report.pdf", Some("/workspace"));
        assert!(
            result.is_err(),
            "absolute path outside workspace should be rejected"
        );
        let err = result.unwrap_err();
        assert!(err.contains("outside the workspace directory"));
    }

    #[test]
    fn test_absolute_path_subdirectory_inside_workspace() {
        let resolved =
            resolve_output_path("/workspace/subdir/out.pdf", Some("/workspace")).unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/subdir/out.pdf"));
    }

    #[test]
    fn test_relative_path_with_workspace() {
        let resolved = resolve_output_path("report.pdf", Some("/workspace")).unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/report.pdf"));
    }

    #[test]
    fn test_relative_path_traversal_blocked() {
        let result = resolve_output_path("../../etc/passwd", Some("/workspace/project"));
        assert!(result.is_err(), "traversal should be blocked");
        let err = result.unwrap_err();
        assert!(err.contains("outside the workspace directory"));
    }

    #[test]
    fn test_relative_path_nested_allowed() {
        let resolved = resolve_output_path("subdir/report.pdf", Some("/workspace")).unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/subdir/report.pdf"));
    }

    #[test]
    fn test_home_tilde_path_no_workspace() {
        let resolved = resolve_output_path("~/documents/report.pdf", None).unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(resolved, home.join("documents/report.pdf"));
    }

    #[test]
    fn test_home_tilde_path_bypasses_workspace_bound() {
        // `~` paths are an explicit user choice to write outside the workspace root.
        let resolved = resolve_output_path("~/documents/report.pdf", Some("/workspace")).unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(resolved, home.join("documents/report.pdf"));
    }

    #[test]
    fn test_dotdot_normalization_in_absolute_path() {
        let resolved =
            resolve_output_path("/workspace/a/../b/report.pdf", Some("/workspace")).unwrap();
        assert_eq!(resolved, PathBuf::from("/workspace/b/report.pdf"));
    }
}
