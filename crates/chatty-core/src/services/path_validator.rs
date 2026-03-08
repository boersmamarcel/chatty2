use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use tokio::fs;

/// Maximum file size allowed for read operations (10MB)
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Validates and restricts file system paths to a workspace root directory.
///
/// All paths are canonicalized to resolve symlinks and `..` sequences,
/// then checked to ensure they remain within the workspace boundary.
pub struct PathValidator {
    workspace_root: PathBuf,
}

impl PathValidator {
    /// Create a new PathValidator with the given workspace root.
    /// The workspace root is canonicalized at creation time.
    pub async fn new(workspace_root: &str) -> Result<Self> {
        let root = PathBuf::from(workspace_root);
        let canonical_root = fs::canonicalize(&root).await.map_err(|e| {
            anyhow!(
                "Failed to canonicalize workspace root '{}': {}",
                workspace_root,
                e
            )
        })?;

        Ok(Self {
            workspace_root: canonical_root,
        })
    }

    /// Validate that a path is within the workspace root.
    /// Returns the canonicalized absolute path on success.
    pub async fn validate(&self, path: &str) -> Result<PathBuf> {
        if path.is_empty() {
            return Err(anyhow!("Path cannot be empty"));
        }

        let requested = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };

        // Canonicalize to resolve symlinks, `.`, `..`, etc.
        let canonical = fs::canonicalize(&requested).await.map_err(|e| {
            anyhow!(
                "Failed to resolve path '{}': {}. The file or directory may not exist.",
                path,
                e
            )
        })?;

        // Check that the canonical path starts with the workspace root
        if !canonical.starts_with(&self.workspace_root) {
            return Err(anyhow!(
                "Access denied: path '{}' is outside the workspace root",
                path
            ));
        }

        Ok(canonical)
    }

    /// Validate a path for a file that may not yet exist (write operations).
    /// Ensures the parent directory exists and is within the workspace root.
    /// Returns the resolved absolute path.
    pub async fn validate_new_path(&self, path: &str) -> Result<PathBuf> {
        if path.is_empty() {
            return Err(anyhow!("Path cannot be empty"));
        }

        let requested = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };

        // The parent directory must exist
        let parent = requested
            .parent()
            .ok_or_else(|| anyhow!("Invalid path: no parent directory for '{}'", path))?;

        if !fs::try_exists(parent).await.unwrap_or(false) {
            return Err(anyhow!(
                "Parent directory does not exist for '{}'. Use create_directory first.",
                path
            ));
        }

        // Canonicalize the parent to check workspace boundary
        let canonical_parent = fs::canonicalize(parent)
            .await
            .map_err(|e| anyhow!("Failed to resolve parent path for '{}': {}", path, e))?;

        if !canonical_parent.starts_with(&self.workspace_root) {
            return Err(anyhow!(
                "Access denied: path '{}' is outside the workspace root",
                path
            ));
        }

        // Return parent + filename (canonical parent with the original filename)
        let filename = requested
            .file_name()
            .ok_or_else(|| anyhow!("Invalid path: no filename in '{}'", path))?;

        Ok(canonical_parent.join(filename))
    }

    /// Validate a path for creating directories (ancestors may not exist).
    /// Ensures the resolved path would be within the workspace root.
    pub async fn validate_mkdir_path(&self, path: &str) -> Result<PathBuf> {
        if path.is_empty() {
            return Err(anyhow!("Path cannot be empty"));
        }

        let requested = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };

        // Walk up to find an existing ancestor and canonicalize it
        let mut check = requested.as_path();
        loop {
            if fs::try_exists(check).await.unwrap_or(false) {
                let canonical = fs::canonicalize(check)
                    .await
                    .map_err(|e| anyhow!("Failed to resolve path '{}': {}", check.display(), e))?;
                if !canonical.starts_with(&self.workspace_root) {
                    return Err(anyhow!(
                        "Access denied: path '{}' is outside the workspace root",
                        path
                    ));
                }
                break;
            }
            check = match check.parent() {
                Some(p) => p,
                None => {
                    return Err(anyhow!(
                        "Access denied: path '{}' is outside the workspace root",
                        path
                    ));
                }
            };
        }

        Ok(requested)
    }

    /// Validate a path that may not exist yet (for glob patterns).
    /// Returns the resolved path without canonicalization.
    #[allow(dead_code)]
    pub async fn validate_parent(&self, path: &str) -> Result<PathBuf> {
        if path.is_empty() {
            return Err(anyhow!("Path cannot be empty"));
        }

        let requested = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.workspace_root.join(path)
        };

        // For glob patterns, validate the parent directory exists and is within workspace
        if let Some(parent) = requested.parent()
            && fs::try_exists(parent).await.unwrap_or(false)
        {
            let canonical_parent = fs::canonicalize(parent)
                .await
                .map_err(|e| anyhow!("Failed to resolve parent path: {}", e))?;
            if !canonical_parent.starts_with(&self.workspace_root) {
                return Err(anyhow!(
                    "Access denied: path '{}' is outside the workspace root",
                    path
                ));
            }
        }

        Ok(requested)
    }

    /// Check that a file is within the size limit.
    pub async fn validate_file_size(&self, path: &Path) -> Result<u64> {
        let metadata = fs::metadata(path).await.map_err(|e| {
            anyhow!(
                "Failed to read file metadata for '{}': {}",
                path.display(),
                e
            )
        })?;

        let size = metadata.len();
        if size > MAX_FILE_SIZE {
            return Err(anyhow!(
                "File '{}' is too large ({} bytes, max {} bytes / 10MB)",
                path.display(),
                size,
                MAX_FILE_SIZE
            ));
        }

        Ok(size)
    }

    /// Get the workspace root path.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn test_validate_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let test_file = tmp.path().join("test.txt");
        fs::write(&test_file, "hello").unwrap();

        let validator = PathValidator::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let result = validator.validate("test.txt").await;
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            tokio::fs::canonicalize(&test_file).await.unwrap()
        );
    }

    #[tokio::test]
    async fn test_validate_absolute_path_inside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let test_file = tmp.path().join("test.txt");
        fs::write(&test_file, "hello").unwrap();

        let validator = PathValidator::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let result = validator.validate(test_file.to_str().unwrap()).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_validate_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let validator = PathValidator::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        // Create a file outside the workspace to traverse to
        let result = validator.validate("../../../etc/passwd").await;
        // This should either fail validation (outside workspace) or fail to resolve
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_rejects_empty_path() {
        let tmp = tempfile::tempdir().unwrap();
        let validator = PathValidator::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let result = validator.validate("").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_nonexistent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let validator = PathValidator::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let result = validator.validate("nonexistent.txt").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_file_size() {
        let tmp = tempfile::tempdir().unwrap();
        let test_file = tmp.path().join("small.txt");
        fs::write(&test_file, "hello").unwrap();

        let validator = PathValidator::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let result = validator.validate_file_size(&test_file).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 5);
    }

    #[tokio::test]
    async fn test_subdirectory_access() {
        let tmp = tempfile::tempdir().unwrap();
        let sub_dir = tmp.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        let test_file = sub_dir.join("test.txt");
        fs::write(&test_file, "hello").unwrap();

        let validator = PathValidator::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let result = validator.validate("subdir/test.txt").await;
        assert!(result.is_ok());
    }
}
