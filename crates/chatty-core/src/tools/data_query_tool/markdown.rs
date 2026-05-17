//! Markdown rendering for query results.
//!
//! Converts a `Vec<duckdb::Row>` into a Markdown table for the LLM,
//! handling truncation, type coercion, and cell escaping. Pure
//! functions only.

use duckdb::Connection;

use super::{ColumnInfo, DataQueryError, MAX_MARKDOWN_CELL_CHARS};

pub(super) fn results_to_markdown(
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
pub(super) fn value_to_string(val: &duckdb::types::Value) -> String {
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

pub(super) fn format_markdown_cell(value: &str) -> (String, bool) {
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

