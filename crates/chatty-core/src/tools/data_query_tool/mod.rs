//! `data_query_tool` — query CSV / Parquet / JSON data files with DuckDB SQL.
//!
//! Exposes a single LLM tool that opens a DuckDB connection over the
//! workspace filesystem, runs a user-provided SQL query, and returns the
//! result as JSON with a row preview.
//!
//! # What lives here
//!
//! - The `DataQueryTool` rig-core tool implementation.
//! - Format detection (CSV / Parquet / JSON / JSONL / Excel) and the
//!   corresponding DuckDB read function.
//! - Result formatting (column metadata + row preview, capped to keep
//!   token usage bounded).
//! - Path safety checks delegated to `FileSystemService`.
//!
//! # What does NOT live here
//!
//! - Filesystem permission policy — `services::filesystem_service`.
//! - Tool registration with the agent — `factories::agent_factory`.

mod markdown;
mod profile;
mod sql;

use markdown::*;
use profile::*;
use sql::*;

use duckdb::Connection;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

use crate::services::filesystem_service::FileSystemService;

const DEFAULT_QUERY_MAX_ROWS: u32 = 20;
const MAX_QUERY_MAX_ROWS: u32 = 10_000;
pub(super) const MAX_MARKDOWN_CELL_CHARS: usize = 80;
const DEFAULT_PROFILE_SAMPLE_ROWS: u32 = 2;
const MAX_PROFILE_SAMPLE_ROWS: u32 = 5;
pub(super) const MAX_PROFILE_COLUMNS: usize = 8;
pub(super) const MAX_PROFILE_IMPORTANT_COLUMNS: usize = 14;
pub(super) const MAX_PROFILE_SAMPLE_COLUMNS: usize = 8;
pub(super) type ProfileDataSummary = (
    Vec<ColumnInfo>,
    u64,
    String,
    Vec<ColumnProfile>,
    Vec<String>,
);

// ─── shared types ───

/// Column metadata returned by both tools.
#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
}

/// Error type shared by both tools.
#[derive(Debug, thiserror::Error)]
pub enum DataQueryError {
    #[error("Query failed: {0}")]
    QueryFailed(String),
    #[error("Path not allowed: {0}")]
    PathNotAllowed(String),
    #[error("File not found: {0}")]
    FileNotFound(String),
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Create an in-memory DuckDB connection sandboxed to the given workspace root.
///
/// Disables external network access and sets the working directory so that
/// relative file paths in SQL resolve within the workspace.
pub(super) fn sandboxed_connection(workspace_root: &str) -> Result<Connection, DataQueryError> {
    let conn = Connection::open_in_memory()
        .map_err(|e| DataQueryError::QueryFailed(format!("Failed to open DuckDB: {}", e)))?;
    let workspace_root = escape_sql_string(workspace_root);

    // Lock DuckDB down to workspace-local files only. This still allows relative
    // reads inside the workspace but blocks arbitrary local file access and
    // prevents the SQL itself from loosening the configuration again.
    conn.execute_batch(&format!(
        "SET autoinstall_known_extensions = false;
             SET autoload_known_extensions = false;
             SET allow_community_extensions = false;
             SET allowed_directories = ['{workspace_root}'];
             SET file_search_path = '{workspace_root}';
             SET enable_external_access = false;
             SET lock_configuration = true;"
    ))
    .map_err(|e| DataQueryError::QueryFailed(format!("Failed to configure sandbox: {}", e)))?;

    Ok(conn)
}

#[derive(Deserialize, Serialize)]
pub struct QueryDataArgs {
    /// SQL query to execute. File paths in the query are relative to the workspace.
    pub query: String,
    /// Maximum number of result rows to return (default: 20).
    pub max_rows: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct QueryDataOutput {
    /// Results formatted as a markdown table.
    pub markdown_table: String,
    /// Number of rows returned.
    pub row_count: usize,
    /// Number of columns in the result.
    pub column_count: usize,
    /// Column metadata (name and type).
    pub columns: Vec<ColumnInfo>,
    /// Whether the result was truncated to max_rows.
    pub truncated: bool,
    /// Optional note (e.g., truncation warning).
    pub note: Option<String>,
}

#[derive(Clone)]
pub struct QueryDataTool {
    service: Arc<FileSystemService>,
}

impl QueryDataTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for QueryDataTool {
    const NAME: &'static str = "query_data";
    type Error = DataQueryError;
    type Args = QueryDataArgs;
    type Output = QueryDataOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "query_data".to_string(),
            description:
                "Run a SQL query against local Parquet, CSV, or JSON files using DuckDB.\n\
                          \n\
                          DuckDB can query files directly by path in SQL:\n\
                          - SELECT * FROM 'data/payments.csv' WHERE year = 2023 LIMIT 5\n\
                          - SELECT * FROM read_csv('/app/data/payments.csv', header=true) LIMIT 5\n\
                          - SELECT * FROM read_parquet('*.parquet')\n\
                          - SELECT COUNT(*), category FROM 'data/payments.csv' GROUP BY category\n\
                          \n\
                          File paths are relative to the workspace directory unless you use an explicit absolute path. \
                          Prefer aggregate queries and small LIMITs for previews. \
                          For benchmark tasks with `/app/data`, use `/app/data/<file>` or `data/<file>`, not bare file names like `payments.csv`. \
                          Supports aggregations, joins, window functions, and all standard SQL."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "SQL query to execute. Reference files by `data/<file>` or explicit workspace paths such as `/app/data/payments.csv`; avoid bare file names."
                    },
                    "max_rows": {
                        "type": "integer",
                        "description": "Maximum number of preview rows to return. Default: 20. Max: 10000."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let max_rows = args
            .max_rows
            .unwrap_or(DEFAULT_QUERY_MAX_ROWS)
            .clamp(1, MAX_QUERY_MAX_ROWS);
        let workspace_root = self.service.workspace_root().to_path_buf();
        let query = args.query.clone();

        info!(query = %args.query, max_rows, "Executing data query");

        let (markdown_table, columns, row_count, truncated, shortened_values) =
            tokio::task::spawn_blocking(move || {
                let workspace_root_str = workspace_root.to_string_lossy().to_string();
                let rewritten_query = rewrite_query_file_literals(&query, &workspace_root)?;
                let conn = sandboxed_connection(&workspace_root_str)?;
                results_to_markdown(&conn, &rewritten_query, max_rows)
            })
            .await
            .map_err(|e| DataQueryError::QueryFailed(format!("Task error: {}", e)))??;

        let note = match (truncated, shortened_values) {
            (true, true) => Some(format!(
                "Results truncated to {} preview rows and long cell values were shortened for display. Use LIMIT, aggregate queries, or more specific WHERE clauses to narrow results.",
                max_rows
            )),
            (true, false) => Some(format!(
                "Results truncated to {} preview rows. Use LIMIT, aggregate queries, or more specific WHERE clauses to narrow results.",
                max_rows
            )),
            (false, true) => Some(
                "Long cell values were shortened for display. Narrow the query or select fewer text-heavy columns for a more detailed preview."
                    .to_string(),
            ),
            (false, false) => None,
        };

        info!(
            row_count,
            column_count = columns.len(),
            truncated,
            "Query completed"
        );

        Ok(QueryDataOutput {
            column_count: columns.len(),
            markdown_table,
            row_count,
            columns,
            truncated,
            note,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════
// DescribeDataTool — Inspect schema and stats of a data file
// ═══════════════════════════════════════════════════════════════════

#[derive(Deserialize, Serialize)]
pub struct DescribeDataArgs {
    /// Path to the data file (relative to workspace). Supports .parquet, .csv, .json files.
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct DescribeDataOutput {
    /// File name.
    pub file_name: String,
    /// File size in bytes.
    pub file_size_bytes: u64,
    /// Detected format (parquet, csv, json).
    pub format: String,
    /// Approximate row count.
    pub row_count: u64,
    /// Column metadata.
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopValue {
    pub value: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnProfile {
    pub name: String,
    pub data_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub null_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_values: Vec<TopValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct ProfileDataArgs {
    /// Path to the data file (relative to workspace). Supports .parquet, .csv, .json files.
    pub path: String,
    /// Number of sample rows to return (default: 2, max: 5).
    pub sample_rows: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct ProfileDataOutput {
    pub file_name: String,
    pub file_size_bytes: u64,
    pub format: String,
    pub row_count: u64,
    pub columns: Vec<ColumnInfo>,
    pub sample_rows_markdown: String,
    pub column_profiles: Vec<ColumnProfile>,
    pub notes: Vec<String>,
}

#[derive(Clone)]
pub struct DescribeDataTool {
    service: Arc<FileSystemService>,
}

#[derive(Clone)]
pub struct ProfileDataTool {
    service: Arc<FileSystemService>,
}

impl ProfileDataTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl DescribeDataTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for DescribeDataTool {
    const NAME: &'static str = "describe_data";
    type Error = DataQueryError;
    type Args = DescribeDataArgs;
    type Output = DescribeDataOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "describe_data".to_string(),
            description: "Inspect the schema and statistics of a local data file.\n\
                         Returns column names, data types, row count, and file size.\n\
                         \n\
                         Supported formats: Parquet (.parquet), CSV (.csv), JSON (.json)\n\
                         \n\
                         Use this to understand the structure of a data file before writing queries."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the data file, relative to workspace root. Supports .parquet, .csv, .json"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Validate path is within workspace
        let canonical = self
            .service
            .resolve_path(&args.path)
            .await
            .map_err(|e| DataQueryError::PathNotAllowed(e.to_string()))?;

        if !canonical.exists() {
            return Err(DataQueryError::FileNotFound(args.path.clone()));
        }

        let file_size_bytes = tokio::fs::metadata(&canonical)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let ext = canonical
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let format = match ext.as_str() {
            "parquet" => "parquet",
            "csv" | "tsv" => "csv",
            "json" | "jsonl" | "ndjson" => "json",
            _ => {
                return Err(DataQueryError::UnsupportedFormat(format!(
                    "Unsupported file extension '.{}'. Supported: .parquet, .csv, .tsv, .json, .jsonl",
                    ext
                )));
            }
        };

        let workspace_root = self.service.workspace_root().to_string_lossy().to_string();
        let file_path_owned = canonical.to_string_lossy().to_string();
        let format_owned = format.to_string();

        let file_name = canonical
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&args.path)
            .to_string();

        let (columns, row_count) = tokio::task::spawn_blocking(move || {
            let conn = sandboxed_connection(&workspace_root)?;

            let escaped = escape_sql_string(&file_path_owned);

            // Get schema via DESCRIBE
            let describe_sql = match format_owned.as_str() {
                "parquet" => format!("DESCRIBE SELECT * FROM read_parquet('{}')", escaped),
                "csv" => format!("DESCRIBE SELECT * FROM read_csv('{}')", escaped),
                "json" => format!("DESCRIBE SELECT * FROM read_json_auto('{}')", escaped),
                _ => unreachable!(),
            };

            let mut columns = Vec::new();
            {
                let mut stmt = conn
                    .prepare(&describe_sql)
                    .map_err(|e| DataQueryError::QueryFailed(e.to_string()))?;
                let rows = stmt
                    .query([])
                    .map_err(|e| DataQueryError::QueryFailed(e.to_string()))?;
                let mut row_iter = rows;
                loop {
                    match row_iter.next() {
                        Ok(Some(row)) => {
                            let name: String = row.get(0).unwrap_or_default();
                            let data_type: String = row.get(1).unwrap_or_default();
                            columns.push(ColumnInfo { name, data_type });
                        }
                        Ok(None) => break,
                        Err(e) => {
                            warn!(error = ?e, "Error reading DESCRIBE result");
                            break;
                        }
                    }
                }
            }

            // Get row count
            let count_sql = match format_owned.as_str() {
                "parquet" => format!("SELECT COUNT(*) FROM read_parquet('{}')", escaped),
                "csv" => format!("SELECT COUNT(*) FROM read_csv('{}')", escaped),
                "json" => format!("SELECT COUNT(*) FROM read_json_auto('{}')", escaped),
                _ => unreachable!(),
            };

            let row_count: u64 = conn
                .query_row(&count_sql, [], |row| row.get(0))
                .unwrap_or(0);

            Ok::<_, DataQueryError>((columns, row_count))
        })
        .await
        .map_err(|e| DataQueryError::QueryFailed(format!("Task error: {}", e)))??;

        info!(
            file = %file_name,
            format,
            row_count,
            columns = columns.len(),
            "Described data file"
        );

        Ok(DescribeDataOutput {
            file_name,
            file_size_bytes,
            format: format.to_string(),
            row_count,
            columns,
        })
    }
}

impl Tool for ProfileDataTool {
    const NAME: &'static str = "profile_data";
    type Error = DataQueryError;
    type Args = ProfileDataArgs;
    type Output = ProfileDataOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "profile_data".to_string(),
            description: "Profile a local CSV, JSON, JSONL, or Parquet file with compact, structured statistics. Returns schema, row count, a tiny sample, numeric min/max/avg/sum, categorical top values, null counts, and notes. Use this early for generic data-analysis tasks before writing custom code or many SQL queries."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the data file, relative to workspace root. Supports .parquet, .csv, .tsv, .json, .jsonl, .ndjson."
                    },
                    "sample_rows": {
                        "type": "integer",
                        "description": "Number of sample rows to return. Default: 2. Max: 5."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self
            .service
            .resolve_path(&args.path)
            .await
            .map_err(|e| DataQueryError::PathNotAllowed(e.to_string()))?;

        if !canonical.exists() {
            return Err(DataQueryError::FileNotFound(args.path.clone()));
        }

        let file_size_bytes = tokio::fs::metadata(&canonical)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let file_name = canonical
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&args.path)
            .to_string();
        let ext = canonical
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let format = data_format_from_extension(&ext)?;
        let workspace_root = self.service.workspace_root().to_string_lossy().to_string();
        let file_path_owned = canonical.to_string_lossy().to_string();
        let sample_rows = args
            .sample_rows
            .unwrap_or(DEFAULT_PROFILE_SAMPLE_ROWS)
            .clamp(1, MAX_PROFILE_SAMPLE_ROWS);

        let (columns, row_count, sample_rows_markdown, column_profiles, mut notes) =
            tokio::task::spawn_blocking(move || {
                profile_data_file(&workspace_root, &file_path_owned, format, sample_rows)
            })
            .await
            .map_err(|e| DataQueryError::QueryFailed(format!("Task error: {}", e)))??;

        if columns.len() > MAX_PROFILE_IMPORTANT_COLUMNS {
            notes.push(format!(
                "Profiled up to {MAX_PROFILE_IMPORTANT_COLUMNS} representative/important columns out of {} to keep output compact.",
                columns.len()
            ));
        }

        Ok(ProfileDataOutput {
            file_name,
            file_size_bytes,
            format: format.to_string(),
            row_count,
            columns,
            sample_rows_markdown,
            column_profiles,
            notes,
        })
    }
}


#[cfg(test)]
mod tests;
