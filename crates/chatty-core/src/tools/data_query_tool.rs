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
const MAX_MARKDOWN_CELL_CHARS: usize = 80;
const DEFAULT_PROFILE_SAMPLE_ROWS: u32 = 2;
const MAX_PROFILE_SAMPLE_ROWS: u32 = 5;
const MAX_PROFILE_COLUMNS: usize = 8;
const MAX_PROFILE_IMPORTANT_COLUMNS: usize = 14;
const MAX_PROFILE_SAMPLE_COLUMNS: usize = 8;
type ProfileDataSummary = (
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
fn sandboxed_connection(workspace_root: &str) -> Result<Connection, DataQueryError> {
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

fn escape_sql_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn rewrite_query_file_literals(sql: &str, workspace_root: &Path) -> Result<String, DataQueryError> {
    let mut rewritten = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\'' {
            rewritten.push(ch);
            continue;
        }

        let mut raw_literal = String::new();
        let mut original_literal = String::from("'");

        loop {
            let Some(next) = chars.next() else {
                return Err(DataQueryError::QueryFailed(
                    "Query contains an unterminated string literal".to_string(),
                ));
            };

            original_literal.push(next);
            if next == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    original_literal.push('\'');
                    raw_literal.push('\'');
                    continue;
                }
                break;
            }

            raw_literal.push(next);
        }

        if let Some(path_literal) = resolve_workspace_file_literal(&raw_literal, workspace_root)? {
            rewritten.push('\'');
            rewritten.push_str(&escape_sql_string(&path_literal));
            rewritten.push('\'');
        } else {
            rewritten.push_str(&original_literal);
        }
    }

    Ok(rewritten)
}

fn resolve_workspace_file_literal(
    literal: &str,
    workspace_root: &Path,
) -> Result<Option<String>, DataQueryError> {
    if !looks_like_file_literal(literal) {
        return Ok(None);
    }

    let requested = Path::new(literal);
    let candidate = if requested.is_absolute() {
        PathBuf::from(requested)
    } else {
        workspace_root.join(requested)
    };
    let resolved = resolve_literal_path(&candidate, workspace_root, literal)?;

    Ok(Some(resolved.to_string_lossy().to_string()))
}

fn looks_like_file_literal(literal: &str) -> bool {
    let trimmed = literal.trim();
    if trimmed.is_empty() || trimmed.contains('\n') {
        return false;
    }

    trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.starts_with('.')
        || trimmed.contains('*')
        || trimmed.contains('?')
        || trimmed
            .rsplit_once('.')
            .map(|(_, ext)| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "csv" | "tsv" | "parquet" | "json" | "jsonl" | "ndjson" | "duckdb" | "db"
                )
            })
            .unwrap_or(false)
}

fn resolve_literal_path(
    candidate: &Path,
    workspace_root: &Path,
    original_literal: &str,
) -> Result<PathBuf, DataQueryError> {
    let resolved = if candidate.exists() && !has_glob_pattern(candidate) {
        std::fs::canonicalize(candidate).map_err(|e| {
            DataQueryError::PathNotAllowed(format!(
                "Failed to resolve path '{original_literal}': {e}"
            ))
        })?
    } else {
        let parent = candidate.parent().unwrap_or(workspace_root);
        let canonical_parent = std::fs::canonicalize(parent).map_err(|e| {
            DataQueryError::PathNotAllowed(format!(
                "Failed to resolve path '{original_literal}': {e}"
            ))
        })?;

        let Some(file_name) = candidate.file_name() else {
            return Err(DataQueryError::PathNotAllowed(format!(
                "Path '{original_literal}' is not a file path"
            )));
        };

        canonical_parent.join(file_name)
    };

    if !resolved.starts_with(workspace_root) {
        return Err(DataQueryError::PathNotAllowed(format!(
            "Access denied: path '{original_literal}' is outside the workspace root"
        )));
    }

    Ok(resolved)
}

fn has_glob_pattern(path: &Path) -> bool {
    path.to_string_lossy()
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | ']'))
}

/// Format DuckDB query results as a markdown table.
///
/// Returns (markdown_string, total_rows_fetched, was_truncated).
fn results_to_markdown(
    conn: &Connection,
    sql: &str,
    max_rows: u32,
) -> Result<(String, Vec<ColumnInfo>, usize, bool, bool), DataQueryError> {
    // Use DESCRIBE to get column metadata before executing the query.
    // Calling column_type() before query() panics for dynamic queries like
    // SELECT * FROM read_parquet(...) because DuckDB needs to scan the file
    // to determine the schema.
    let describe_sql = format!("DESCRIBE ({})", sql);
    let columns: Vec<ColumnInfo> = {
        let mut stmt = conn
            .prepare(&describe_sql)
            .map_err(|e| DataQueryError::QueryFailed(e.to_string()))?;
        let rows = stmt
            .query([])
            .map_err(|e| DataQueryError::QueryFailed(e.to_string()))?;
        let mut row_iter = rows;
        let mut cols = Vec::new();
        loop {
            match row_iter.next() {
                Ok(Some(row)) => {
                    let name: String = row.get(0).unwrap_or_default();
                    let data_type: String = row.get(1).unwrap_or_default();
                    cols.push(ColumnInfo { name, data_type });
                }
                Ok(None) => break,
                Err(e) => return Err(DataQueryError::QueryFailed(e.to_string())),
            }
        }
        cols
    };

    let column_count = columns.len();

    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| DataQueryError::QueryFailed(e.to_string()))?;

    let mut rows_data: Vec<Vec<String>> = Vec::new();
    // Fetch max_rows + 1 to detect truncation
    let limit = max_rows as usize + 1;

    let rows = stmt
        .query([])
        .map_err(|e| DataQueryError::QueryFailed(e.to_string()))?;

    // Use a manual approach to iterate rows
    let mut row_iter = rows;
    loop {
        match row_iter.next() {
            Ok(Some(row)) => {
                if rows_data.len() >= limit {
                    break;
                }
                let mut row_values = Vec::with_capacity(column_count);
                for i in 0..column_count {
                    let val: String = row
                        .get::<_, duckdb::types::Value>(i)
                        .map(|v| value_to_string(&v))
                        .unwrap_or_else(|_| "NULL".to_string());
                    row_values.push(val);
                }
                rows_data.push(row_values);
            }
            Ok(None) => break,
            Err(e) => return Err(DataQueryError::QueryFailed(e.to_string())),
        }
    }

    let truncated = rows_data.len() > max_rows as usize;
    if truncated {
        rows_data.truncate(max_rows as usize);
    }

    let total_rows = rows_data.len();

    // Build markdown table
    let mut md = String::new();
    if columns.is_empty() {
        return Ok((String::from("(no columns)"), columns, 0, false, false));
    }

    // Header row
    md.push('|');
    for col in &columns {
        md.push_str(&format!(" {} |", col.name));
    }
    md.push('\n');

    // Separator
    md.push('|');
    for _ in &columns {
        md.push_str(" --- |");
    }
    md.push('\n');

    // Data rows
    let mut shortened_values = false;
    for row in &rows_data {
        md.push('|');
        for (i, _col) in columns.iter().enumerate() {
            let val = row.get(i).map(|s| s.as_str()).unwrap_or("");
            let (display, shortened) = format_markdown_cell(val);
            shortened_values |= shortened;
            md.push_str(&format!(" {} |", display));
        }
        md.push('\n');
    }

    Ok((md, columns, total_rows, truncated, shortened_values))
}

/// Convert a DuckDB Value to a display string.
fn value_to_string(val: &duckdb::types::Value) -> String {
    match val {
        duckdb::types::Value::Null => "NULL".to_string(),
        duckdb::types::Value::Boolean(b) => b.to_string(),
        duckdb::types::Value::TinyInt(n) => n.to_string(),
        duckdb::types::Value::SmallInt(n) => n.to_string(),
        duckdb::types::Value::Int(n) => n.to_string(),
        duckdb::types::Value::BigInt(n) => n.to_string(),
        duckdb::types::Value::HugeInt(n) => n.to_string(),
        duckdb::types::Value::UTinyInt(n) => n.to_string(),
        duckdb::types::Value::USmallInt(n) => n.to_string(),
        duckdb::types::Value::UInt(n) => n.to_string(),
        duckdb::types::Value::UBigInt(n) => n.to_string(),
        duckdb::types::Value::Float(f) => f.to_string(),
        duckdb::types::Value::Double(f) => f.to_string(),
        duckdb::types::Value::Text(s) => s.clone(),
        duckdb::types::Value::Blob(b) => format!("<blob {} bytes>", b.len()),
        _ => format!("{:?}", val),
    }
}

fn format_markdown_cell(value: &str) -> (String, bool) {
    let mut normalized = value.replace(['\r', '\n', '\t'], " ");
    let shortened = normalized.chars().count() > MAX_MARKDOWN_CELL_CHARS;
    if shortened {
        normalized = normalized
            .chars()
            .take(MAX_MARKDOWN_CELL_CHARS)
            .collect::<String>();
        normalized.push_str("...");
    }

    (normalized.replace('|', "\\|"), shortened)
}

// ═══════════════════════════════════════════════════════════════════
// QueryDataTool — Run SQL queries against local data files
// ═══════════════════════════════════════════════════════════════════

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

fn data_format_from_extension(ext: &str) -> Result<&'static str, DataQueryError> {
    match ext {
        "parquet" => Ok("parquet"),
        "csv" | "tsv" => Ok("csv"),
        "json" | "jsonl" | "ndjson" => Ok("json"),
        _ => Err(DataQueryError::UnsupportedFormat(format!(
            "Unsupported file extension '.{}'. Supported: .parquet, .csv, .tsv, .json, .jsonl, .ndjson",
            ext
        ))),
    }
}

fn profile_data_file(
    workspace_root: &str,
    file_path: &str,
    format: &str,
    sample_rows: u32,
) -> Result<ProfileDataSummary, DataQueryError> {
    let conn = sandboxed_connection(workspace_root)?;
    let escaped = escape_sql_string(file_path);
    let source = data_source_sql(format, &escaped);

    let columns = describe_source(&conn, &source)?;
    let row_count = count_source_rows(&conn, &source)?;
    let sample_columns = columns
        .iter()
        .take(MAX_PROFILE_SAMPLE_COLUMNS)
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>();
    let sample_select = if sample_columns.is_empty() {
        "*".to_string()
    } else {
        sample_columns.join(", ")
    };
    let sample_sql = format!("SELECT {sample_select} FROM {source} LIMIT {sample_rows}");
    let (sample_rows_markdown, _, _, _, shortened_values) =
        results_to_markdown(&conn, &sample_sql, sample_rows)?;

    let mut notes = Vec::new();
    if shortened_values {
        notes.push("Long sample values were shortened for display.".to_string());
    }
    if columns.len() > MAX_PROFILE_SAMPLE_COLUMNS {
        notes.push(format!(
            "Sample rows show the first {MAX_PROFILE_SAMPLE_COLUMNS} of {} columns.",
            columns.len()
        ));
    }

    let profile_columns = select_profile_columns(&columns);
    let mut profiles = Vec::new();
    for column in &profile_columns {
        profiles.push(profile_column(&conn, &source, column));
    }
    if columns.len() > MAX_PROFILE_COLUMNS && profile_columns.len() > MAX_PROFILE_COLUMNS {
        let names = profile_columns
            .iter()
            .skip(MAX_PROFILE_COLUMNS)
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(format!(
            "Added likely important low-cardinality columns beyond the first {MAX_PROFILE_COLUMNS}: {names}."
        ));
    }

    Ok((columns, row_count, sample_rows_markdown, profiles, notes))
}

fn select_profile_columns(columns: &[ColumnInfo]) -> Vec<ColumnInfo> {
    let mut selected = Vec::new();
    for column in columns.iter().take(MAX_PROFILE_COLUMNS) {
        selected.push(column.clone());
    }
    if selected.len() >= MAX_PROFILE_IMPORTANT_COLUMNS {
        return selected;
    }

    for column in columns.iter().skip(MAX_PROFILE_COLUMNS) {
        if selected.len() >= MAX_PROFILE_IMPORTANT_COLUMNS {
            break;
        }
        if is_likely_important_low_cardinality_column(column)
            && !selected.iter().any(|existing| existing.name == column.name)
        {
            selected.push(column.clone());
        }
    }
    selected
}

fn is_likely_important_low_cardinality_column(column: &ColumnInfo) -> bool {
    let name = column.name.to_ascii_lowercase();
    let data_type = column.data_type.to_ascii_uppercase();
    if is_complex_type(&data_type) {
        return false;
    }
    data_type.contains("BOOLEAN")
        || name == "aci"
        || name.ends_with("_aci")
        || name.contains("scheme")
        || name.contains("status")
        || name.contains("fraud")
        || name.contains("refus")
        || name.contains("credit")
        || name.contains("debit")
        || name.contains("country")
        || name.contains("interaction")
        || name.contains("device")
        || name.contains("category")
        || name.contains("type")
        || name.contains("bucket")
        || name.contains("level")
}

fn describe_source(conn: &Connection, source: &str) -> Result<Vec<ColumnInfo>, DataQueryError> {
    let mut stmt = conn
        .prepare(&format!("DESCRIBE SELECT * FROM {source}"))
        .map_err(|e| DataQueryError::QueryFailed(e.to_string()))?;
    let rows = stmt
        .query([])
        .map_err(|e| DataQueryError::QueryFailed(e.to_string()))?;
    let mut row_iter = rows;
    let mut columns = Vec::new();
    loop {
        match row_iter.next() {
            Ok(Some(row)) => {
                let name: String = row.get(0).unwrap_or_default();
                let data_type: String = row.get(1).unwrap_or_default();
                columns.push(ColumnInfo { name, data_type });
            }
            Ok(None) => break,
            Err(e) => return Err(DataQueryError::QueryFailed(e.to_string())),
        }
    }
    Ok(columns)
}

fn count_source_rows(conn: &Connection, source: &str) -> Result<u64, DataQueryError> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {source}"), [], |row| {
        row.get(0)
    })
    .map_err(|e| DataQueryError::QueryFailed(e.to_string()))
}

fn profile_column(conn: &Connection, source: &str, column: &ColumnInfo) -> ColumnProfile {
    let ident = quote_identifier(&column.name);
    let mut notes = Vec::new();
    let null_count = conn
        .query_row(
            &format!("SELECT COUNT(*) - COUNT({ident}) FROM {source}"),
            [],
            |row| row.get(0),
        )
        .map_err(|e| notes.push(format!("null_count unavailable: {e}")))
        .ok()
        .filter(|count| *count > 0);

    let (min, max, average, sum) = if is_numeric_type(&column.data_type) {
        match conn.query_row(
            &format!("SELECT MIN({ident}), MAX({ident}), AVG({ident}), SUM({ident}) FROM {source}"),
            [],
            |row| {
                let min = row
                    .get::<_, duckdb::types::Value>(0)
                    .map(|v| value_to_string(&v))
                    .ok();
                let max = row
                    .get::<_, duckdb::types::Value>(1)
                    .map(|v| value_to_string(&v))
                    .ok();
                let avg = row.get::<_, Option<f64>>(2).ok().flatten();
                let sum = row.get::<_, Option<f64>>(3).ok().flatten();
                Ok((min, max, avg, sum))
            },
        ) {
            Ok(stats) => stats,
            Err(e) => {
                notes.push(format!("numeric stats unavailable: {e}"));
                (None, None, None, None)
            }
        }
    } else {
        (None, None, None, None)
    };

    let top_values = if should_collect_top_values(&column.data_type) {
        top_values_for_column(conn, source, &ident, &mut notes)
    } else {
        Vec::new()
    };

    ColumnProfile {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        null_count,
        min,
        max,
        average,
        sum,
        top_values,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join("; "))
        },
    }
}

fn top_values_for_column(
    conn: &Connection,
    source: &str,
    ident: &str,
    notes: &mut Vec<String>,
) -> Vec<TopValue> {
    let sql = format!(
        "SELECT CAST({ident} AS VARCHAR) AS value, COUNT(*) AS count \
         FROM {source} GROUP BY 1 ORDER BY count DESC, value ASC LIMIT 5"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) => {
            notes.push(format!("top values unavailable: {e}"));
            return Vec::new();
        }
    };
    let rows = match stmt.query([]) {
        Ok(rows) => rows,
        Err(e) => {
            notes.push(format!("top values unavailable: {e}"));
            return Vec::new();
        }
    };
    let mut row_iter = rows;
    let mut values = Vec::new();
    loop {
        match row_iter.next() {
            Ok(Some(row)) => {
                let value = row
                    .get::<_, duckdb::types::Value>(0)
                    .map(|v| value_to_string(&v))
                    .unwrap_or_else(|_| "NULL".to_string());
                let count = row.get::<_, u64>(1).unwrap_or(0);
                let (value, _) = format_markdown_cell(&value);
                values.push(TopValue { value, count });
            }
            Ok(None) => break,
            Err(e) => {
                notes.push(format!("top values unavailable: {e}"));
                break;
            }
        }
    }
    values
}

fn data_source_sql(format: &str, escaped_path: &str) -> String {
    match format {
        "parquet" => format!("read_parquet('{escaped_path}')"),
        "csv" => format!("read_csv('{escaped_path}')"),
        "json" => format!("read_json_auto('{escaped_path}')"),
        _ => unreachable!(),
    }
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn is_numeric_type(data_type: &str) -> bool {
    let ty = data_type.to_ascii_uppercase();
    if is_complex_type(&ty) {
        return false;
    }
    [
        "TINYINT",
        "SMALLINT",
        "INTEGER",
        "BIGINT",
        "HUGEINT",
        "UTINYINT",
        "USMALLINT",
        "UINTEGER",
        "UBIGINT",
        "FLOAT",
        "DOUBLE",
        "DECIMAL",
        "NUMERIC",
        "REAL",
    ]
    .iter()
    .any(|needle| ty.contains(needle))
}

fn should_collect_top_values(data_type: &str) -> bool {
    let ty = data_type.to_ascii_uppercase();
    if is_complex_type(&ty) {
        return false;
    }
    ty.contains("VARCHAR")
        || ty.contains("TEXT")
        || ty.contains("BOOLEAN")
        || ty.contains("ENUM")
        || ty.contains("DATE")
        || ty.contains("TIME")
}

fn is_complex_type(upper_data_type: &str) -> bool {
    upper_data_type.contains("[]")
        || upper_data_type.contains("LIST")
        || upper_data_type.contains("STRUCT")
        || upper_data_type.contains("MAP")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rig::tool::Tool;

    use crate::services::filesystem_service::FileSystemService;

    #[tokio::test]
    async fn profile_data_returns_compact_generic_summary() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            data_dir.join("sales.csv"),
            "category,amount,flag\nbook,10,true\nbook,20,false\ngame,30,true\n",
        )
        .unwrap();

        let service = Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ProfileDataTool::new(service);

        let output = tool
            .call(ProfileDataArgs {
                path: "data/sales.csv".to_string(),
                sample_rows: Some(2),
            })
            .await
            .unwrap();

        assert_eq!(output.file_name, "sales.csv");
        assert_eq!(output.row_count, 3);
        assert_eq!(output.columns.len(), 3);
        assert!(output.sample_rows_markdown.contains("book"));

        let amount = output
            .column_profiles
            .iter()
            .find(|profile| profile.name == "amount")
            .unwrap();
        assert_eq!(amount.min.as_deref(), Some("10"));
        assert_eq!(amount.max.as_deref(), Some("30"));
        assert_eq!(amount.sum, Some(60.0));

        let category = output
            .column_profiles
            .iter()
            .find(|profile| profile.name == "category")
            .unwrap();
        assert!(
            category
                .top_values
                .iter()
                .any(|value| value.value == "book" && value.count == 2)
        );
    }

    #[test]
    fn profile_skips_top_values_for_complex_types() {
        assert!(!should_collect_top_values("VARCHAR[]"));
        assert!(!should_collect_top_values("STRUCT(name VARCHAR)"));
        assert!(!is_numeric_type("BIGINT[]"));
        assert!(should_collect_top_values("VARCHAR"));
        assert!(is_numeric_type("BIGINT"));
    }

    #[test]
    fn profile_selects_important_columns_beyond_first_eight() {
        let columns = vec![
            ("psp_reference", "BIGINT"),
            ("merchant", "VARCHAR"),
            ("card_scheme", "VARCHAR"),
            ("year", "BIGINT"),
            ("hour_of_day", "BIGINT"),
            ("minute_of_hour", "BIGINT"),
            ("day_of_year", "BIGINT"),
            ("is_credit", "BOOLEAN"),
            ("eur_amount", "DOUBLE"),
            ("email_address", "VARCHAR"),
            ("has_fraudulent_dispute", "BOOLEAN"),
            ("is_refused_by_adyen", "BOOLEAN"),
            ("aci", "VARCHAR"),
            ("acquirer_country", "VARCHAR"),
        ]
        .into_iter()
        .map(|(name, data_type)| ColumnInfo {
            name: name.to_string(),
            data_type: data_type.to_string(),
        })
        .collect::<Vec<_>>();

        let selected = select_profile_columns(&columns);
        let names = selected
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"has_fraudulent_dispute"));
        assert!(names.contains(&"is_refused_by_adyen"));
        assert!(names.contains(&"aci"));
        assert!(names.contains(&"acquirer_country"));
        assert!(!names.contains(&"email_address"));
    }

    #[tokio::test]
    async fn query_data_reads_workspace_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            data_dir.join("sales.csv"),
            "category,amount\nbook,10\ngame,30\n",
        )
        .unwrap();

        let service = Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = QueryDataTool::new(service);

        let output = tool
            .call(QueryDataArgs {
                query: "SELECT * FROM 'data/sales.csv' ORDER BY amount".to_string(),
                max_rows: Some(10),
            })
            .await
            .unwrap();

        assert_eq!(output.row_count, 2);
        assert_eq!(output.column_count, 2);
        assert!(output.markdown_table.contains("book"));
        assert!(output.markdown_table.contains("game"));
    }

    #[tokio::test]
    async fn query_data_rejects_files_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), "secret\nvalue\n").unwrap();

        let service = Arc::new(
            FileSystemService::new(workspace.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = QueryDataTool::new(service);
        let outside_path = escape_sql_string(&outside.path().to_string_lossy());

        let result = tool
            .call(QueryDataArgs {
                query: format!("SELECT * FROM read_csv('{outside_path}', header=true)"),
                max_rows: Some(10),
            })
            .await;

        match result {
            Err(DataQueryError::PathNotAllowed(message)) => {
                assert!(message.contains("outside the workspace root"));
            }
            Err(DataQueryError::QueryFailed(message)) => {
                assert!(
                    message.contains("Permission")
                        || message.contains("disabled")
                        || message.contains("external")
                );
            }
            other => panic!("expected permission failure, got {other:?}"),
        }
    }
}
