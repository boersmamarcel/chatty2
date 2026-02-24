use anyhow::{Result, anyhow};
use regex::Regex;
use serde::Serialize;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::{debug, warn};

/// A single match from a ripgrep search
#[derive(Debug, Serialize)]
pub struct SearchMatch {
    /// File path relative to workspace root
    pub path: String,
    /// Line number (1-indexed)
    pub line_number: u64,
    /// The matching line content
    pub line: String,
}

/// Result of a code search operation
#[derive(Debug, Serialize)]
pub struct SearchResult {
    /// All matching lines
    pub matches: Vec<SearchMatch>,
    /// Total number of matches returned
    pub count: usize,
    /// Whether results were truncated due to max_results limit
    pub truncated: bool,
}

/// Result of a glob file search
#[derive(Debug, Serialize)]
pub struct GlobFilesResult {
    /// Matching file paths relative to workspace root
    pub files: Vec<String>,
    /// Total number of matches
    pub count: usize,
}

/// A single symbol definition match
#[derive(Debug, Serialize)]
pub struct DefinitionMatch {
    /// File path relative to workspace root
    pub path: String,
    /// Line number (1-indexed)
    pub line_number: u64,
    /// The line containing the definition
    pub line: String,
    /// Language of the file (e.g., "rust", "javascript", "python")
    pub language: String,
}

/// Result of a symbol definition search
#[derive(Debug, Serialize)]
pub struct DefinitionResult {
    /// All found definitions
    pub definitions: Vec<DefinitionMatch>,
    /// Total number of definitions found
    pub count: usize,
}

/// Service for code search and navigation operations.
///
/// Provides full-text search via ripgrep, glob-based file finding,
/// and regex-based symbol definition lookup. All operations are
/// restricted to the workspace root for security.
pub struct CodeSearchService {
    workspace_root: PathBuf,
}

impl CodeSearchService {
    /// Create a new CodeSearchService with the given workspace root.
    pub fn new(workspace_root: &str) -> Result<Self> {
        let workspace_root = PathBuf::from(workspace_root)
            .canonicalize()
            .map_err(|e| anyhow!("Invalid workspace root '{}': {}", workspace_root, e))?;
        Ok(Self { workspace_root })
    }

    /// Search for a pattern in the workspace using ripgrep.
    ///
    /// Runs `rg --json` as a subprocess and parses structured output.
    /// Results are limited to `max_results` (default 100).
    /// Requires the `rg` (ripgrep) CLI tool to be installed.
    pub async fn search_code(
        &self,
        pattern: &str,
        case_insensitive: bool,
        file_type: Option<&str>,
        max_results: usize,
    ) -> Result<SearchResult> {
        let mut args: Vec<String> = vec!["--json".to_string()];

        if case_insensitive {
            args.push("--ignore-case".to_string());
        }

        if let Some(ft) = file_type {
            args.push("--type".to_string());
            args.push(ft.to_string());
        }

        args.push(pattern.to_string());
        args.push(self.workspace_root.to_string_lossy().to_string());

        debug!(
            pattern = %pattern,
            workspace = %self.workspace_root.display(),
            case_insensitive,
            file_type,
            "Running ripgrep search"
        );

        let output = Command::new("rg")
            .args(&args)
            .output()
            .await
            .map_err(|e| {
                anyhow!(
                    "Failed to run ripgrep: {}. Ensure 'rg' (ripgrep) is installed.",
                    e
                )
            })?;

        // ripgrep exits with 0 (matches), 1 (no matches), or 2+ (error)
        if let Some(code) = output.status.code() {
            if code >= 2 {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow!("ripgrep error: {}", stderr.trim()));
            }
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut matches = self.parse_ripgrep_json(&stdout);

        let truncated = matches.len() > max_results;
        if truncated {
            matches.truncate(max_results);
        }

        let count = matches.len();
        Ok(SearchResult {
            matches,
            count,
            truncated,
        })
    }

    /// Parse the JSON output from `rg --json` into `SearchMatch` structs.
    fn parse_ripgrep_json(&self, json_output: &str) -> Vec<SearchMatch> {
        let mut matches = Vec::new();

        for line in json_output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = ?e, "Failed to parse ripgrep JSON line");
                    continue;
                }
            };

            // ripgrep --json emits multiple message types: begin, match, end, summary
            // We only care about "match"
            if value.get("type").and_then(|t| t.as_str()) != Some("match") {
                continue;
            }

            let data = match value.get("data") {
                Some(d) => d,
                None => continue,
            };

            // Extract file path
            let abs_path = data
                .get("path")
                .and_then(|p| p.get("text"))
                .and_then(|t| t.as_str())
                .map(PathBuf::from);

            let rel_path = match abs_path {
                Some(p) => {
                    if let Ok(rel) = p.strip_prefix(&self.workspace_root) {
                        rel.to_string_lossy().to_string()
                    } else {
                        p.to_string_lossy().to_string()
                    }
                }
                None => continue,
            };

            // Extract line number
            let line_number = data
                .get("line_number")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);

            // Extract matching line text, strip trailing newline
            let line_text = data
                .get("lines")
                .and_then(|l| l.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_string();

            matches.push(SearchMatch {
                path: rel_path,
                line_number,
                line: line_text,
            });
        }

        matches
    }

    /// Find files matching a glob pattern within the workspace.
    ///
    /// Results are limited to 100 matches to prevent excessive output.
    pub async fn glob_files(&self, pattern: &str) -> Result<GlobFilesResult> {
        use std::path::Path;

        let workspace_root = self.workspace_root.clone();

        // Build full pattern anchored to workspace root
        let full_pattern = if Path::new(pattern).is_absolute() {
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

        debug!(pattern = %full_pattern, "Executing glob file search");

        let files: Vec<String> = glob::glob(&full_pattern)
            .map_err(|e| anyhow!("Invalid glob pattern '{}': {}", pattern, e))?
            .filter_map(|entry| match entry {
                Ok(path) => {
                    if let Ok(canonical) = path.canonicalize() {
                        if canonical.starts_with(&workspace_root) {
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
            })
            .take(100)
            .collect();

        let count = files.len();
        Ok(GlobFilesResult { files, count })
    }

    /// Find definitions of a symbol in the workspace.
    ///
    /// Searches Rust, JavaScript/TypeScript, and Python files using
    /// regex patterns that match common definition syntax for each language.
    pub async fn find_definition(&self, symbol: &str) -> Result<DefinitionResult> {
        let escaped = regex::escape(symbol);

        // Language definitions: (language_name, file_extensions, regex_pattern)
        let language_specs: &[(&str, &[&str], &str)] = &[
            (
                "rust",
                &["rs"],
                // fn, struct, enum, trait, type alias, impl, mod, const, static, macro_rules!
                &format!(
                    r"(?:^|\s)(?:pub\s+)?(?:async\s+)?(?:fn|struct|enum|trait|type|impl|mod|const|static|macro_rules!)\s+{escaped}(?:\s|<|\(|\{{|!)"
                ),
            ),
            (
                "javascript",
                &["js", "ts", "jsx", "tsx", "mjs", "cjs"],
                // function, class, const/let/var assignment, type alias, interface, enum
                &format!(
                    r"(?:^|\s)(?:export\s+)?(?:default\s+)?(?:async\s+)?(?:function|class|const|let|var|type|interface|enum)\s+{escaped}(?:\s|=|\(|<|\{{)"
                ),
            ),
            (
                "python",
                &["py"],
                // def and class
                &format!(r"(?:^|\s)(?:def|class)\s+{escaped}(?:\s|\(|:)"),
            ),
        ];

        let mut definitions = Vec::new();

        for (lang, extensions, pattern_str) in language_specs {
            let pattern = Regex::new(pattern_str).map_err(|e| {
                anyhow!("Failed to compile definition regex for {}: {}", lang, e)
            })?;

            let files = self.collect_files_by_extensions(extensions).await;

            for file_path in files {
                let abs_path = self.workspace_root.join(&file_path);
                let content = match tokio::fs::read_to_string(&abs_path).await {
                    Ok(c) => c,
                    Err(_) => continue, // skip unreadable files silently
                };

                for (line_idx, line_content) in content.lines().enumerate() {
                    if pattern.is_match(line_content) {
                        definitions.push(DefinitionMatch {
                            path: file_path.clone(),
                            line_number: (line_idx + 1) as u64,
                            line: line_content.trim().to_string(),
                            language: lang.to_string(),
                        });
                    }
                }
            }
        }

        let count = definitions.len();
        Ok(DefinitionResult { definitions, count })
    }

    /// Collect all files within the workspace that match any of the given extensions.
    async fn collect_files_by_extensions(&self, extensions: &[&str]) -> Vec<String> {
        let mut all_files = Vec::new();

        for ext in extensions {
            let pattern = format!("{}/**/*.{}", self.workspace_root.display(), ext);

            let files: Vec<String> = glob::glob(&pattern)
                .map(|paths| {
                    paths
                        .filter_map(|entry| {
                            entry.ok().and_then(|path| {
                                if let Ok(canonical) = path.canonicalize() {
                                    if canonical.starts_with(&self.workspace_root) {
                                        canonical
                                            .strip_prefix(&self.workspace_root)
                                            .ok()
                                            .map(|p| p.to_string_lossy().to_string())
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();

            all_files.extend(files);
        }

        all_files
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn test_glob_files_basic() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("main.rs"), "fn main(){}").unwrap();
        fs::write(tmp.path().join("lib.rs"), "pub mod lib;").unwrap();
        fs::write(tmp.path().join("readme.md"), "# Readme").unwrap();

        let service = CodeSearchService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.glob_files("**/*.rs").await.unwrap();

        assert_eq!(result.count, 2);
        assert!(result.files.iter().any(|f| f.contains("main.rs")));
        assert!(result.files.iter().any(|f| f.contains("lib.rs")));
    }

    #[tokio::test]
    async fn test_glob_files_no_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let service = CodeSearchService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.glob_files("**/*.xyz").await.unwrap();
        assert_eq!(result.count, 0);
        assert!(result.files.is_empty());
    }

    #[tokio::test]
    async fn test_glob_files_traversal_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let service = CodeSearchService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.glob_files("/etc/passwd").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_find_definition_rust() {
        let tmp = tempfile::tempdir().unwrap();
        let code = r#"
pub fn my_function(x: i32) -> i32 {
    x + 1
}

pub struct MyStruct {
    field: String,
}
"#;
        fs::write(tmp.path().join("lib.rs"), code).unwrap();

        let service = CodeSearchService::new(tmp.path().to_str().unwrap()).unwrap();

        let result = service.find_definition("my_function").await.unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.definitions[0].language, "rust");
        assert!(result.definitions[0].line.contains("my_function"));

        let result = service.find_definition("MyStruct").await.unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.definitions[0].language, "rust");
        assert!(result.definitions[0].line.contains("MyStruct"));
    }

    #[tokio::test]
    async fn test_find_definition_python() {
        let tmp = tempfile::tempdir().unwrap();
        let code = r#"
def my_function(x):
    return x + 1

class MyClass:
    pass
"#;
        fs::write(tmp.path().join("main.py"), code).unwrap();

        let service = CodeSearchService::new(tmp.path().to_str().unwrap()).unwrap();

        let result = service.find_definition("my_function").await.unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.definitions[0].language, "python");

        let result = service.find_definition("MyClass").await.unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.definitions[0].language, "python");
    }

    #[tokio::test]
    async fn test_find_definition_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("empty.rs"), "// nothing here").unwrap();

        let service = CodeSearchService::new(tmp.path().to_str().unwrap()).unwrap();
        let result = service.find_definition("nonexistent_symbol").await.unwrap();
        assert_eq!(result.count, 0);
        assert!(result.definitions.is_empty());
    }

    #[tokio::test]
    async fn test_parse_ripgrep_json() {
        let tmp = tempfile::tempdir().unwrap();
        let service = CodeSearchService::new(tmp.path().to_str().unwrap()).unwrap();

        // Simulate ripgrep JSON output
        let json = format!(
            r#"{{"type":"begin","data":{{"path":{{"text":"{}/src/main.rs"}}}}}}
{{"type":"match","data":{{"path":{{"text":"{}/src/main.rs"}},"line_number":5,"lines":{{"text":"fn main() {{\n"}},"submatches":[]}}}}
{{"type":"end","data":{{"path":{{"text":"{}/src/main.rs"}},"binary_offset":null,"stats":{{}}}}}}
{{"type":"summary","data":{{"elapsed_total":{{"secs":0,"nanos":0,"human":"0s"}},"stats":{{}}}}}}
"#,
            tmp.path().display(),
            tmp.path().display(),
            tmp.path().display()
        );

        let matches = service.parse_ripgrep_json(&json);
        assert_eq!(matches.len(), 1);
        assert!(matches[0].path.contains("main.rs"));
        assert_eq!(matches[0].line_number, 5);
        assert!(matches[0].line.contains("fn main"));
    }
}
