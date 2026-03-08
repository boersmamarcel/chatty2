use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::chatty::services::filesystem_service::FileSystemService;

/// Error type for filesystem tool operations
#[derive(Debug, thiserror::Error)]
pub enum FileSystemToolError {
    #[error("Filesystem error: {0}")]
    OperationError(#[from] anyhow::Error),
}

// ─── read_file tool ───

#[derive(Deserialize, Serialize)]
pub struct ReadFileArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ReadFileOutput {
    pub content: String,
    pub path: String,
}

#[derive(Clone)]
pub struct ReadFileTool {
    service: Arc<FileSystemService>,
}

impl ReadFileTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";
    type Error = FileSystemToolError;
    type Args = ReadFileArgs;
    type Output = ReadFileOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read the contents of a text file within the workspace. \
                         Returns the file contents as a string. \
                         Files must be within the workspace directory and under 10MB. \
                         For binary files (images, PDFs), use read_binary instead.\n\
                         \n\
                         Examples:\n\
                         - Read source code: {\"path\": \"src/main.rs\"}\n\
                         - Read config: {\"path\": \"config.json\"}\n\
                         - Read nested file: {\"path\": \"src/utils/helpers.rs\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read, relative to the workspace root or absolute within workspace"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let content = self.service.read_file(&args.path).await?;
        Ok(ReadFileOutput {
            content,
            path: args.path,
        })
    }
}

// ─── read_binary tool ───

#[derive(Deserialize, Serialize)]
pub struct ReadBinaryArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ReadBinaryOutput {
    pub data: String,
    pub path: String,
    pub encoding: String,
}

#[derive(Clone)]
pub struct ReadBinaryTool {
    service: Arc<FileSystemService>,
}

impl ReadBinaryTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for ReadBinaryTool {
    const NAME: &'static str = "read_binary";
    type Error = FileSystemToolError;
    type Args = ReadBinaryArgs;
    type Output = ReadBinaryOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "read_binary".to_string(),
            description: "Read a binary file (images, PDFs, etc.) and return its contents as base64-encoded data. \
                         Files must be within the workspace directory and under 10MB.\n\
                         \n\
                         Examples:\n\
                         - Read image: {\"path\": \"assets/logo.png\"}\n\
                         - Read PDF: {\"path\": \"docs/report.pdf\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the binary file to read, relative to the workspace root or absolute within workspace"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let data = self.service.read_binary(&args.path).await?;
        Ok(ReadBinaryOutput {
            data,
            path: args.path,
            encoding: "base64".to_string(),
        })
    }
}

// ─── list_directory tool ───

#[derive(Deserialize, Serialize)]
pub struct ListDirectoryArgs {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ListDirectoryOutput {
    pub entries: Vec<DirectoryEntryOutput>,
    pub path: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct DirectoryEntryOutput {
    pub name: String,
    pub entry_type: String,
    pub size: u64,
}

#[derive(Clone)]
pub struct ListDirectoryTool {
    service: Arc<FileSystemService>,
}

impl ListDirectoryTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for ListDirectoryTool {
    const NAME: &'static str = "list_directory";
    type Error = FileSystemToolError;
    type Args = ListDirectoryArgs;
    type Output = ListDirectoryOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "list_directory".to_string(),
            description: "List the contents of a directory within the workspace. \
                         Returns file and directory names with type and size metadata. \
                         Results are sorted with directories first, then files alphabetically.\n\
                         \n\
                         Examples:\n\
                         - List workspace root: {\"path\": \".\"}\n\
                         - List subdirectory: {\"path\": \"src\"}\n\
                         - List nested directory: {\"path\": \"src/components\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory to list, relative to the workspace root or absolute within workspace"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let entries = self.service.list_directory(&args.path).await?;
        let count = entries.len();
        let entry_outputs = entries
            .into_iter()
            .map(|e| DirectoryEntryOutput {
                name: e.name,
                entry_type: e.entry_type,
                size: e.size,
            })
            .collect();

        Ok(ListDirectoryOutput {
            entries: entry_outputs,
            path: args.path,
            count,
        })
    }
}

// ─── glob_search tool ───

#[derive(Deserialize, Serialize)]
pub struct GlobSearchArgs {
    pub pattern: String,
}

#[derive(Debug, Serialize)]
pub struct GlobSearchOutput {
    pub matches: Vec<String>,
    pub count: usize,
    pub pattern: String,
}

#[derive(Clone)]
pub struct GlobSearchTool {
    service: Arc<FileSystemService>,
}

impl GlobSearchTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for GlobSearchTool {
    const NAME: &'static str = "glob_search";
    type Error = FileSystemToolError;
    type Args = GlobSearchArgs;
    type Output = GlobSearchOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "glob_search".to_string(),
            description: "Search for files matching a glob pattern within the workspace. \
                         Returns matching file paths relative to the workspace root. \
                         Results are limited to 1000 matches.\n\
                         \n\
                         Pattern syntax:\n\
                         - `*` matches any sequence of characters in a file/dir name\n\
                         - `**` matches any number of directories (recursive)\n\
                         - `?` matches a single character\n\
                         - `[abc]` matches one of the characters\n\
                         \n\
                         Examples:\n\
                         - Find all Rust files: {\"pattern\": \"**/*.rs\"}\n\
                         - Find all test files: {\"pattern\": \"**/test_*.py\"}\n\
                         - Find configs: {\"pattern\": \"*.{json,toml,yaml}\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files against, relative to the workspace root"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let result = self.service.glob_search(&args.pattern).await?;
        Ok(GlobSearchOutput {
            matches: result.matches,
            count: result.count,
            pattern: args.pattern,
        })
    }
}
