use anyhow::{Result, anyhow};
use serde::Serialize;
use std::path::Path;
use tracing::{debug, warn};

use super::path_validator::PathValidator;

/// Metadata for a directory entry
#[derive(Debug, Serialize)]
pub struct DirectoryEntry {
    /// File or directory name
    pub name: String,
    /// "file" or "directory"
    pub entry_type: String,
    /// File size in bytes (0 for directories)
    pub size: u64,
}

/// Result of a glob search
#[derive(Debug, Serialize)]
pub struct GlobResult {
    /// Matching file paths (relative to workspace)
    pub matches: Vec<String>,
    /// Total number of matches
    pub count: usize,
}

/// File system read operations service.
///
/// All operations are workspace-restricted via PathValidator.
/// Files are subject to a 10MB size limit.
pub struct FileSystemService {
    validator: PathValidator,
}

impl FileSystemService {
    /// Create a new FileSystemService with the given workspace root.
    pub fn new(workspace_root: &str) -> Result<Self> {
        let validator = PathValidator::new(workspace_root)?;
        Ok(Self { validator })
    }

    /// Read a text file and return its contents as a string.
    ///
    /// The file must be within the workspace root and under 10MB.
    pub async fn read_file(&self, path: &str) -> Result<String> {
        let canonical = self.validator.validate(path)?;
        self.validator.validate_file_size(&canonical)?;

        debug!(path = %canonical.display(), "Reading text file");

        tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            anyhow!(
                "Failed to read file '{}': {}. The file may be binary - use read_binary instead.",
                path,
                e
            )
        })
    }

    /// Read a binary file and return its contents as base64-encoded data.
    ///
    /// Suitable for images and PDFs. The file must be within the workspace root and under 10MB.
    pub async fn read_binary(&self, path: &str) -> Result<String> {
        let canonical = self.validator.validate(path)?;
        self.validator.validate_file_size(&canonical)?;

        debug!(path = %canonical.display(), "Reading binary file");

        let bytes = tokio::fs::read(&canonical)
            .await
            .map_err(|e| anyhow!("Failed to read binary file '{}': {}", path, e))?;

        Ok(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &bytes,
        ))
    }

    /// List contents of a directory.
    ///
    /// Returns entries with name, type, and size metadata.
    pub async fn list_directory(&self, path: &str) -> Result<Vec<DirectoryEntry>> {
        let canonical = self.validator.validate(path)?;

        if !canonical.is_dir() {
            return Err(anyhow!("'{}' is not a directory", path));
        }

        debug!(path = %canonical.display(), "Listing directory");

        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&canonical)
            .await
            .map_err(|e| anyhow!("Failed to read directory '{}': {}", path, e))?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|e| anyhow!("Failed to read directory entry: {}", e))?
        {
            let metadata = entry.metadata().await;
            let (entry_type, size) = match metadata {
                Ok(meta) => {
                    let et = if meta.is_dir() { "directory" } else { "file" };
                    (et.to_string(), if meta.is_file() { meta.len() } else { 0 })
                }
                Err(e) => {
                    warn!(error = ?e, "Failed to read metadata for {:?}", entry.path());
                    ("unknown".to_string(), 0)
                }
            };

            entries.push(DirectoryEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                entry_type,
                size,
            });
        }

        // Sort entries: directories first, then files, alphabetically within each group
        entries.sort_by(|a, b| {
            let type_order = |t: &str| if t == "directory" { 0 } else { 1 };
            type_order(&a.entry_type)
                .cmp(&type_order(&b.entry_type))
                .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Ok(entries)
    }

    /// Search for files matching a glob pattern within the workspace.
    ///
    /// Supports patterns like `**/*.rs`, `src/*.txt`, etc.
    /// Results are limited to 1000 matches to prevent excessive output.
    pub async fn glob_search(&self, pattern: &str) -> Result<GlobResult> {
        let workspace_root = self.validator.workspace_root().to_path_buf();

        // Build the full glob pattern anchored to the workspace root
        let full_pattern = if Path::new(pattern).is_absolute() {
            // Validate absolute patterns are within workspace
            if !pattern.starts_with(workspace_root.to_str().unwrap_or("")) {
                return Err(anyhow!(
                    "Access denied: glob pattern '{}' is outside the workspace root",
                    pattern
                ));
            }
            pattern.to_string()
        } else {
            format!("{}/{}", workspace_root.display(), pattern)
        };

        debug!(pattern = %full_pattern, "Executing glob search");

        let matches: Vec<String> = glob::glob(&full_pattern)
            .map_err(|e| anyhow!("Invalid glob pattern '{}': {}", pattern, e))?
            .filter_map(|entry| {
                match entry {
                    Ok(path) => {
                        // Ensure result is within workspace (extra safety)
                        if let Ok(canonical) = path.canonicalize() {
                            if canonical.starts_with(&workspace_root) {
                                // Return path relative to workspace root
                                canonical
                                    .strip_prefix(&workspace_root)
                                    .ok()
                                    .map(|p| p.to_string_lossy().to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    Err(e) => {
                        warn!(error = ?e, "Glob entry error");
                        None
                    }
                }
            })
            .take(1000) // Limit results
            .collect();

        let count = matches.len();
        Ok(GlobResult { matches, count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn test_read_file() {
        let tmp = tempfile::tempdir().unwrap();
        let test_file = tmp.path().join("test.txt");
        fs::write(&test_file, "Hello, world!").unwrap();

        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let content = service.read_file("test.txt").await.unwrap();
        assert_eq!(content, "Hello, world!");
    }

    #[tokio::test]
    async fn test_read_file_in_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let sub_dir = tmp.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();
        let test_file = sub_dir.join("test.txt");
        fs::write(&test_file, "nested content").unwrap();

        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let content = service.read_file("subdir/test.txt").await.unwrap();
        assert_eq!(content, "nested content");
    }

    #[tokio::test]
    async fn test_read_file_traversal_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.read_file("../../../etc/passwd").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let test_file = tmp.path().join("test.bin");
        fs::write(&test_file, &[0u8, 1, 2, 3, 255]).unwrap();

        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let content = service.read_binary("test.bin").await.unwrap();
        // Verify it's valid base64
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &content).unwrap();
        assert_eq!(decoded, vec![0u8, 1, 2, 3, 255]);
    }

    #[tokio::test]
    async fn test_list_directory() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("file1.txt"), "a").unwrap();
        fs::write(tmp.path().join("file2.txt"), "bb").unwrap();
        fs::create_dir(tmp.path().join("subdir")).unwrap();

        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let entries = service.list_directory(".").await.unwrap();

        assert_eq!(entries.len(), 3);
        // Directories should come first
        assert_eq!(entries[0].name, "subdir");
        assert_eq!(entries[0].entry_type, "directory");
        // Then files alphabetically
        assert_eq!(entries[1].name, "file1.txt");
        assert_eq!(entries[1].entry_type, "file");
        assert_eq!(entries[1].size, 1);
        assert_eq!(entries[2].name, "file2.txt");
        assert_eq!(entries[2].size, 2);
    }

    #[tokio::test]
    async fn test_list_directory_not_a_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("file.txt"), "a").unwrap();

        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.list_directory("file.txt").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_glob_search() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("main.rs"), "fn main(){}").unwrap();
        fs::write(tmp.path().join("lib.rs"), "pub mod lib;").unwrap();
        fs::write(tmp.path().join("readme.md"), "# Readme").unwrap();
        let sub_dir = tmp.path().join("src");
        fs::create_dir(&sub_dir).unwrap();
        fs::write(sub_dir.join("utils.rs"), "pub fn util(){}").unwrap();

        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.glob_search("**/*.rs").await.unwrap();

        assert_eq!(result.count, 3);
        assert!(result.matches.iter().any(|m| m.contains("main.rs")));
        assert!(result.matches.iter().any(|m| m.contains("lib.rs")));
        assert!(result.matches.iter().any(|m| m.contains("utils.rs")));
    }

    #[tokio::test]
    async fn test_glob_search_no_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.glob_search("**/*.xyz").await.unwrap();
        assert_eq!(result.count, 0);
        assert!(result.matches.is_empty());
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let service = FileSystemService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.read_file("nonexistent.txt").await;
        assert!(result.is_err());
    }
}
