use duckdb::Connection;
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::services::filesystem_service::FileSystemService;
use crate::tools::ToolError;

const DEFAULT_MAX_FILES: usize = 60;
const HARD_MAX_FILES: usize = 120;
const DEFAULT_SAMPLE_ROWS: u32 = 2;
const MAX_SAMPLE_ROWS: u32 = 3;
const MAX_SAMPLE_COLUMNS: usize = 8;
const MAX_HEADINGS: usize = 40;
const MAX_JSON_KEYS: usize = 24;
const MAX_CELL_CHARS: usize = 80;

#[derive(Debug, Deserialize, Serialize)]
pub struct FileStructureArgs {
    /// Directory to inspect, relative to the workspace root. Defaults to the workspace root.
    pub path: Option<String>,
    /// Maximum number of files to inspect. Defaults to 60, capped at 120.
    pub max_files: Option<usize>,
    /// Number of preview rows for data files. Defaults to 2, capped at 3.
    pub sample_rows: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct FileStructureOutput {
    pub root: String,
    pub files_seen: usize,
    pub entries: Vec<FileStructureEntry>,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct FileStructureEntry {
    pub path: String,
    pub kind: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_count: Option<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<SimpleColumn>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sample_rows: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub markdown_headings: Vec<MarkdownHeading>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_shape: Option<JsonShapeSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SimpleColumn {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Serialize)]
pub struct MarkdownHeading {
    pub level: u8,
    pub title: String,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct JsonShapeSummary {
    pub top_level: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub array_len: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sample_object_keys: Vec<String>,
}

#[derive(Clone)]
pub struct FileStructureTool {
    service: Arc<FileSystemService>,
}

impl FileStructureTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for FileStructureTool {
    const NAME: &'static str = "file_structure_detector";
    type Error = ToolError;
    type Args = FileStructureArgs;
    type Output = FileStructureOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Inspect the workspace or a subdirectory and return a compact file structure map. \
                          For CSV/JSON/Parquet files it returns DuckDB schema, row count, and a tiny preview; \
                          for Markdown it returns heading outlines with line numbers; for JSON it also reports top-level shape. \
                          Use this before reading manuals or profiling many files in data-analysis tasks."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to inspect, relative to workspace root. Defaults to workspace root."
                    },
                    "max_files": {
                        "type": "integer",
                        "description": "Maximum files to inspect. Default: 60. Max: 120."
                    },
                    "sample_rows": {
                        "type": "integer",
                        "description": "Preview rows for data files. Default: 2. Max: 3."
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let requested_path = args.path.unwrap_or_else(|| ".".to_string());
        let root = self
            .service
            .resolve_path(&requested_path)
            .await
            .map_err(|e| ToolError::OperationFailed(e.to_string()))?;
        if !root.exists() {
            return Err(ToolError::OperationFailed(format!(
                "Path not found: {requested_path}"
            )));
        }
        if !root.is_dir() {
            return Err(ToolError::OperationFailed(format!(
                "Path is not a directory: {requested_path}"
            )));
        }

        let workspace_root = self.service.workspace_root().to_path_buf();
        let max_files = args
            .max_files
            .unwrap_or(DEFAULT_MAX_FILES)
            .clamp(1, HARD_MAX_FILES);
        let sample_rows = args
            .sample_rows
            .unwrap_or(DEFAULT_SAMPLE_ROWS)
            .clamp(1, MAX_SAMPLE_ROWS);

        tokio::task::spawn_blocking(move || {
            inspect_directory(&workspace_root, &root, max_files, sample_rows)
                .map_err(|e| ToolError::OperationFailed(e.to_string()))
        })
        .await
        .map_err(|e| ToolError::OperationFailed(format!("Task error: {e}")))?
    }
}

fn inspect_directory(
    workspace_root: &Path,
    root: &Path,
    max_files: usize,
    sample_rows: u32,
) -> anyhow::Result<FileStructureOutput> {
    let files = collect_files(root, max_files + 1)?;
    let truncated = files.len() > max_files;
    let inspected = files.into_iter().take(max_files).collect::<Vec<_>>();
    let mut entries = Vec::with_capacity(inspected.len());
    for path in inspected {
        entries.push(inspect_file(workspace_root, &path, sample_rows));
    }

    let mut notes = Vec::new();
    if truncated {
        notes.push(format!(
            "File listing truncated to {max_files} files; inspect a narrower path for more detail."
        ));
    }

    Ok(FileStructureOutput {
        root: relative_path(workspace_root, root),
        files_seen: entries.len(),
        entries,
        notes,
    })
}

fn collect_files(root: &Path, limit: usize) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut dirs = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = dirs.pop_front() {
        let mut children = std::fs::read_dir(&dir)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        children.sort();
        for path in children {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name.starts_with('.') {
                continue;
            }
            if path.is_dir() {
                dirs.push_back(path);
            } else if path.is_file() {
                files.push(path);
                if files.len() >= limit {
                    return Ok(files);
                }
            }
        }
    }
    Ok(files)
}

fn inspect_file(workspace_root: &Path, path: &Path, sample_rows: u32) -> FileStructureEntry {
    let size_bytes = std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut entry = FileStructureEntry {
        path: relative_path(workspace_root, path),
        kind: file_kind(&ext).to_string(),
        size_bytes,
        row_count: None,
        columns: Vec::new(),
        sample_rows: Vec::new(),
        markdown_headings: Vec::new(),
        json_shape: None,
        note: None,
    };

    match ext.as_str() {
        "csv" | "tsv" | "parquet" | "json" | "jsonl" | "ndjson" => {
            if matches!(ext.as_str(), "json" | "jsonl" | "ndjson") {
                entry.json_shape = summarize_json_shape(path).ok();
            }
            match inspect_data_file(path, &ext, sample_rows) {
                Ok((row_count, columns, rows)) => {
                    entry.row_count = Some(row_count);
                    entry.columns = columns;
                    entry.sample_rows = rows;
                }
                Err(e) => {
                    entry.note = Some(format!("data summary unavailable: {e}"));
                }
            }
        }
        "md" | "markdown" => match summarize_markdown(path) {
            Ok(headings) => entry.markdown_headings = headings,
            Err(e) => entry.note = Some(format!("markdown summary unavailable: {e}")),
        },
        _ => {}
    }

    entry
}

fn inspect_data_file(
    path: &Path,
    ext: &str,
    sample_rows: u32,
) -> anyhow::Result<(u64, Vec<SimpleColumn>, Vec<Vec<String>>)> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "SET autoinstall_known_extensions = false;
         SET autoload_known_extensions = false;
         SET allow_community_extensions = false;",
    )?;
    let escaped = path.to_string_lossy().replace('\'', "''");
    let source = match ext {
        "parquet" => format!("read_parquet('{escaped}')"),
        "json" | "jsonl" | "ndjson" => format!("read_json_auto('{escaped}')"),
        _ => format!("read_csv('{escaped}')"),
    };

    let columns = describe_source(&conn, &source)?;
    let row_count = conn.query_row(&format!("SELECT COUNT(*) FROM {source}"), [], |row| {
        row.get(0)
    })?;
    let sample_columns = columns
        .iter()
        .take(MAX_SAMPLE_COLUMNS)
        .map(|column| quote_identifier(&column.name))
        .collect::<Vec<_>>();
    let sample_select = if sample_columns.is_empty() {
        "*".to_string()
    } else {
        sample_columns.join(", ")
    };
    let rows = sample_source_rows(
        &conn,
        &format!("SELECT {sample_select} FROM {source} LIMIT {sample_rows}"),
        columns.len().min(MAX_SAMPLE_COLUMNS),
    )?;

    Ok((row_count, columns, rows))
}

fn describe_source(conn: &Connection, source: &str) -> anyhow::Result<Vec<SimpleColumn>> {
    let mut stmt = conn.prepare(&format!("DESCRIBE SELECT * FROM {source}"))?;
    let rows = stmt.query([])?;
    let mut row_iter = rows;
    let mut columns = Vec::new();
    while let Some(row) = row_iter.next()? {
        columns.push(SimpleColumn {
            name: row.get(0).unwrap_or_default(),
            data_type: row.get(1).unwrap_or_default(),
        });
    }
    Ok(columns)
}

fn sample_source_rows(
    conn: &Connection,
    sql: &str,
    column_count: usize,
) -> anyhow::Result<Vec<Vec<String>>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query([])?;
    let mut row_iter = rows;
    let mut out = Vec::new();
    while let Some(row) = row_iter.next()? {
        let mut values = Vec::with_capacity(column_count);
        for idx in 0..column_count {
            let value = row
                .get::<_, duckdb::types::Value>(idx)
                .map(|value| value_to_compact_string(&value))
                .unwrap_or_else(|_| "NULL".to_string());
            values.push(value);
        }
        out.push(values);
    }
    Ok(out)
}

fn summarize_markdown(path: &Path) -> anyhow::Result<Vec<MarkdownHeading>> {
    let text = std::fs::read_to_string(path)?;
    let mut headings = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let level = trimmed.chars().take_while(|ch| *ch == '#').count();
        if (1..=6).contains(&level) && trimmed.chars().nth(level) == Some(' ') {
            headings.push(MarkdownHeading {
                level: level as u8,
                title: trimmed[level + 1..].trim().chars().take(120).collect(),
                line: idx + 1,
            });
            if headings.len() >= MAX_HEADINGS {
                break;
            }
        }
    }
    Ok(headings)
}

fn summarize_json_shape(path: &Path) -> anyhow::Result<JsonShapeSummary> {
    let text = std::fs::read_to_string(path)?;
    let value: JsonValue = serde_json::from_str(&text)?;
    Ok(match value {
        JsonValue::Array(items) => {
            let sample_object_keys = items
                .iter()
                .find_map(|item| match item {
                    JsonValue::Object(map) => Some(
                        map.keys()
                            .take(MAX_JSON_KEYS)
                            .map(ToString::to_string)
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            JsonShapeSummary {
                top_level: "array".to_string(),
                array_len: Some(items.len()),
                keys: Vec::new(),
                sample_object_keys,
            }
        }
        JsonValue::Object(map) => JsonShapeSummary {
            top_level: "object".to_string(),
            array_len: None,
            keys: map
                .keys()
                .take(MAX_JSON_KEYS)
                .map(ToString::to_string)
                .collect(),
            sample_object_keys: Vec::new(),
        },
        other => JsonShapeSummary {
            top_level: json_type_name(&other).to_string(),
            array_len: None,
            keys: Vec::new(),
            sample_object_keys: Vec::new(),
        },
    })
}

fn file_kind(ext: &str) -> &'static str {
    match ext {
        "csv" | "tsv" => "table_csv",
        "parquet" => "table_parquet",
        "json" | "jsonl" | "ndjson" => "data_json",
        "md" | "markdown" => "markdown",
        "txt" => "text",
        _ => "file",
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches('/')
        .to_string()
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn value_to_compact_string(value: &duckdb::types::Value) -> String {
    let raw = match value {
        duckdb::types::Value::Null => "NULL".to_string(),
        duckdb::types::Value::Boolean(value) => value.to_string(),
        duckdb::types::Value::TinyInt(value) => value.to_string(),
        duckdb::types::Value::SmallInt(value) => value.to_string(),
        duckdb::types::Value::Int(value) => value.to_string(),
        duckdb::types::Value::BigInt(value) => value.to_string(),
        duckdb::types::Value::HugeInt(value) => value.to_string(),
        duckdb::types::Value::UTinyInt(value) => value.to_string(),
        duckdb::types::Value::USmallInt(value) => value.to_string(),
        duckdb::types::Value::UInt(value) => value.to_string(),
        duckdb::types::Value::UBigInt(value) => value.to_string(),
        duckdb::types::Value::Float(value) => value.to_string(),
        duckdb::types::Value::Double(value) => value.to_string(),
        duckdb::types::Value::Text(value) => value.clone(),
        duckdb::types::Value::Blob(value) => format!("<blob {} bytes>", value.len()),
        _ => format!("{value:?}"),
    };
    compact_string(&raw)
}

fn compact_string(value: &str) -> String {
    let normalized = value
        .replace(['\n', '\r', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.chars().count() <= MAX_CELL_CHARS {
        normalized
    } else {
        format!(
            "{}...",
            normalized
                .chars()
                .take(MAX_CELL_CHARS.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn json_type_name(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn detects_markdown_and_data_structure() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::write(
            dir.path().join("manual.md"),
            "# Manual\n\n## Fees\nText\n\n### Formula\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("data").join("payments.csv"),
            "merchant,amount,flag\nA,10,True\nB,20,False\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("data").join("merchant_data.json"),
            r#"[{"merchant":"A","capture_delay":"1","account_type":"R"}]"#,
        )
        .unwrap();

        let service = Arc::new(
            FileSystemService::new(dir.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = FileStructureTool::new(service);
        let output = tool
            .call(FileStructureArgs {
                path: None,
                max_files: Some(10),
                sample_rows: Some(2),
            })
            .await
            .unwrap();

        assert_eq!(output.files_seen, 3);
        let manual = output
            .entries
            .iter()
            .find(|entry| entry.path == "manual.md")
            .unwrap();
        assert_eq!(manual.markdown_headings.len(), 3);

        let payments = output
            .entries
            .iter()
            .find(|entry| entry.path == "data/payments.csv")
            .unwrap();
        assert_eq!(payments.row_count, Some(2));
        assert!(payments.columns.iter().any(|column| column.name == "flag"));

        let json = output
            .entries
            .iter()
            .find(|entry| entry.path == "data/merchant_data.json")
            .unwrap();
        assert_eq!(json.json_shape.as_ref().unwrap().top_level, "array");
        assert!(
            json.json_shape
                .as_ref()
                .unwrap()
                .sample_object_keys
                .contains(&"merchant".to_string())
        );
    }
}
