use duckdb::Connection;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

use crate::services::filesystem_service::FileSystemService;

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

    // Block extension auto-downloading without disabling local filesystem access.
    // `enable_external_access = false` would also block reading local files, so we use
    // more granular settings instead.
    conn.execute_batch(
        "SET autoinstall_known_extensions = false;
         SET autoload_known_extensions = false;
         SET allow_community_extensions = false;",
    )
    .map_err(|e| DataQueryError::QueryFailed(format!("Failed to configure sandbox: {}", e)))?;

    // Set working directory so relative paths resolve within workspace
    conn.execute_batch(&format!(
        "SET file_search_path = '{}';",
        workspace_root.replace('\'', "''")
    ))
    .map_err(|e| DataQueryError::QueryFailed(format!("Failed to set file_search_path: {}", e)))?;

    Ok(conn)
}

/// Format DuckDB query results as a markdown table.
///
/// Returns (markdown_string, total_rows_fetched, was_truncated).
fn results_to_markdown(
    conn: &Connection,
    sql: &str,
    max_rows: u32,
) -> Result<(String, Vec<ColumnInfo>, usize, bool), DataQueryError> {
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
        return Ok((String::from("(no columns)"), columns, 0, false));
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
    for row in &rows_data {
        md.push('|');
        for (i, _col) in columns.iter().enumerate() {
            let val = row.get(i).map(|s| s.as_str()).unwrap_or("");
            md.push_str(&format!(" {} |", val));
        }
        md.push('\n');
    }

    Ok((md, columns, total_rows, truncated))
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

// ═══════════════════════════════════════════════════════════════════
// QueryDataTool — Run SQL queries against local data files
// ═══════════════════════════════════════════════════════════════════

#[derive(Deserialize, Serialize)]
pub struct QueryDataArgs {
    /// SQL query to execute. File paths in the query are relative to the workspace.
    pub query: String,
    /// Maximum number of result rows to return (default: 100).
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
                         - SELECT * FROM 'data.parquet' WHERE year > 2023\n\
                         - SELECT * FROM read_csv('sales.csv', header=true)\n\
                         - SELECT * FROM read_parquet('*.parquet')\n\
                         - SELECT COUNT(*), category FROM 'data.parquet' GROUP BY category\n\
                         \n\
                         File paths are relative to the workspace directory. \
                         Supports aggregations, joins, window functions, and all standard SQL."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "SQL query to execute. Reference files by relative path, e.g. SELECT * FROM 'data/sales.parquet'"
                    },
                    "max_rows": {
                        "type": "integer",
                        "description": "Maximum number of result rows to return. Default: 100. Max: 10000."
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let max_rows = args.max_rows.unwrap_or(100).min(10000);
        let workspace_root = self.service.workspace_root().to_string_lossy().to_string();
        let query = args.query.clone();

        info!(query = %args.query, max_rows, "Executing data query");

        let (markdown_table, columns, row_count, truncated) =
            tokio::task::spawn_blocking(move || {
                let conn = sandboxed_connection(&workspace_root)?;
                results_to_markdown(&conn, &query, max_rows)
            })
            .await
            .map_err(|e| DataQueryError::QueryFailed(format!("Task error: {}", e)))??;

        let note = if truncated {
            Some(format!(
                "Results truncated to {} rows. Use LIMIT or more specific WHERE clauses to narrow results.",
                max_rows
            ))
        } else {
            None
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

#[derive(Clone)]
pub struct DescribeDataTool {
    service: Arc<FileSystemService>,
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

            let escaped = file_path_owned.replace('\'', "''");

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
