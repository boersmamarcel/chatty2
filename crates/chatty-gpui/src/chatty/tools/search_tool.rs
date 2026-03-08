use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::chatty::services::search_service::{
    CodeSearchService, DefinitionResult, GlobFilesResult, SearchResult,
};

/// Error type for search tool operations
#[derive(Debug, thiserror::Error)]
pub enum SearchToolError {
    #[error("Search error: {0}")]
    OperationError(#[from] anyhow::Error),
}

// ─── search_code tool ───

#[derive(Deserialize, Serialize)]
pub struct SearchCodeArgs {
    pub pattern: String,
    pub case_insensitive: Option<bool>,
    pub file_type: Option<String>,
    pub max_results: Option<usize>,
}

/// Tool that searches the workspace using ripgrep full-text search
#[derive(Clone)]
pub struct SearchCodeTool {
    service: Arc<CodeSearchService>,
}

impl SearchCodeTool {
    pub fn new(service: Arc<CodeSearchService>) -> Self {
        Self { service }
    }
}

impl Tool for SearchCodeTool {
    const NAME: &'static str = "search_code";
    type Error = SearchToolError;
    type Args = SearchCodeArgs;
    type Output = SearchResult;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "search_code".to_string(),
            description: "Search for a text pattern or regex in the workspace using ripgrep. \
                         Returns matching lines with file paths and line numbers. \
                         Requires 'rg' (ripgrep) to be installed. \
                         Results are limited to max_results (default 100).\n\
                         \n\
                         Examples:\n\
                         - Search for a function: {\"pattern\": \"fn main\"}\n\
                         - Case-insensitive search: {\"pattern\": \"TODO\", \"case_insensitive\": true}\n\
                         - Search only Rust files: {\"pattern\": \"use std\", \"file_type\": \"rust\"}\n\
                         - Limit results: {\"pattern\": \"error\", \"max_results\": 20}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "The search pattern (literal text or regex)"
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "Whether to search case-insensitively (default: false)"
                    },
                    "file_type": {
                        "type": "string",
                        "description": "Filter by file type (e.g., 'rust', 'python', 'js', 'ts', 'cpp', 'go'). Uses ripgrep's --type flag."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum number of results to return (default: 100)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let max_results = args.max_results.unwrap_or(100);
        let result = self
            .service
            .search_code(
                &args.pattern,
                args.case_insensitive.unwrap_or(false),
                args.file_type.as_deref(),
                max_results,
            )
            .await?;
        Ok(result)
    }
}

// ─── find_files tool ───

#[derive(Deserialize, Serialize)]
pub struct FindFilesArgs {
    pub pattern: String,
}

/// Tool that finds files matching a glob pattern in the workspace
#[derive(Clone)]
pub struct FindFilesTool {
    service: Arc<CodeSearchService>,
}

impl FindFilesTool {
    pub fn new(service: Arc<CodeSearchService>) -> Self {
        Self { service }
    }
}

impl Tool for FindFilesTool {
    const NAME: &'static str = "find_files";
    type Error = SearchToolError;
    type Args = FindFilesArgs;
    type Output = GlobFilesResult;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "find_files".to_string(),
            description: "Find files matching a glob pattern within the workspace. \
                         Returns matching file paths relative to the workspace root. \
                         Results are limited to 100 matches.\n\
                         \n\
                         Pattern syntax:\n\
                         - `*` matches any sequence of characters in a file/dir name\n\
                         - `**` matches any number of directories (recursive)\n\
                         - `?` matches a single character\n\
                         \n\
                         Examples:\n\
                         - Find all Rust files: {\"pattern\": \"**/*.rs\"}\n\
                         - Find test files: {\"pattern\": \"**/test_*.py\"}\n\
                         - Find config files: {\"pattern\": \"*.toml\"}"
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
        let result = self.service.glob_files(&args.pattern).await?;
        Ok(result)
    }
}

// ─── find_definition tool ───

#[derive(Deserialize, Serialize)]
pub struct FindDefinitionArgs {
    pub symbol: String,
}

/// Tool that finds symbol definitions using regex-based lookup
#[derive(Clone)]
pub struct FindDefinitionTool {
    service: Arc<CodeSearchService>,
}

impl FindDefinitionTool {
    pub fn new(service: Arc<CodeSearchService>) -> Self {
        Self { service }
    }
}

impl Tool for FindDefinitionTool {
    const NAME: &'static str = "find_definition";
    type Error = SearchToolError;
    type Args = FindDefinitionArgs;
    type Output = DefinitionResult;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "find_definition".to_string(),
            description: "Find definitions of a symbol (function, class, struct, etc.) \
                         in the workspace. Searches Rust, JavaScript/TypeScript, and Python files \
                         using regex-based pattern matching.\n\
                         \n\
                         Examples:\n\
                         - Find a Rust function: {\"symbol\": \"parse_config\"}\n\
                         - Find a Python class: {\"symbol\": \"FileManager\"}\n\
                         - Find a TypeScript interface: {\"symbol\": \"UserConfig\"}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "The symbol name to search for (function, class, struct, trait, etc.)"
                    }
                },
                "required": ["symbol"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let result = self.service.find_definition(&args.symbol).await?;
        Ok(result)
    }
}
