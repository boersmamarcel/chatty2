//! Structured table previews for UI rendering (inline cards + artifact panel).
//!
//! Query results and file previews normalize to [`TablePreview`]. Caps keep
//! both LLM payloads and GPUI layouts bounded.

use serde::{Deserialize, Serialize};

use super::markdown::{format_markdown_cell, results_to_markdown};
use super::profile::{data_format_from_extension, data_source_sql};
use super::sql::escape_sql_string;
use super::{ColumnInfo, DataQueryError, sandboxed_connection};

/// Maximum rows shown in UI table views (inline + panel).
pub const UI_TABLE_MAX_ROWS: usize = 50;
/// Maximum columns shown in UI table views.
pub const UI_TABLE_MAX_COLS: usize = 12;
/// Rows visible in the compact inline card before internal scroll.
pub const INLINE_TABLE_PREVIEW_ROWS: usize = 5;
/// Fixed max height (px) for inline table preview scroll area.
pub const INLINE_TABLE_MAX_HEIGHT_PX: f32 = 200.0;
/// Default row fetch for lazy file previews in the artifact panel.
pub const FILE_PREVIEW_MAX_ROWS: u32 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TableSource {
    Query { sql: String },
    File { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TablePreview {
    pub title: String,
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Vec<String>>,
    pub row_count: usize,
    pub truncated: bool,
    pub source: TableSource,
}

impl TablePreview {
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    pub fn summary_label(&self) -> String {
        format!("{} rows × {} cols", self.row_count, self.column_count())
    }
}

/// Build a capped UI preview from raw query rows (full cell values).
pub fn table_preview_from_query(
    sql: String,
    columns: Vec<ColumnInfo>,
    rows: Vec<Vec<String>>,
    row_count: usize,
    truncated: bool,
) -> TablePreview {
    let mut preview = TablePreview {
        title: "query_data".into(),
        columns,
        rows,
        row_count,
        truncated,
        source: TableSource::Query { sql },
    };
    cap_table_preview_for_ui(&mut preview);
    preview
}

fn cap_table_preview_for_ui(preview: &mut TablePreview) {
    if preview.columns.len() > UI_TABLE_MAX_COLS {
        preview.columns.truncate(UI_TABLE_MAX_COLS);
        preview.truncated = true;
    }
    for row in &mut preview.rows {
        if row.len() > preview.columns.len() {
            row.truncate(preview.columns.len());
            preview.truncated = true;
        }
        for cell in row.iter_mut() {
            let (display, _) = format_markdown_cell(cell);
            *cell = display;
        }
    }
    if preview.rows.len() > UI_TABLE_MAX_ROWS {
        preview.rows.truncate(UI_TABLE_MAX_ROWS);
        preview.truncated = true;
    }
}

/// Lazy-load a bounded preview for CSV / TSV / Parquet files via sandboxed DuckDB.
pub fn load_file_table_preview(
    workspace_root: &str,
    file_path: &str,
    max_rows: u32,
) -> Result<TablePreview, DataQueryError> {
    let path = std::path::Path::new(file_path);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let format = data_format_from_extension(&ext)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(file_path)
        .to_string();
    let display_path = file_path.to_string();

    let conn = sandboxed_connection(workspace_root)?;
    let escaped = escape_sql_string(file_path);
    let source = data_source_sql(format, &escaped);
    let sql = format!("SELECT * FROM {source} LIMIT {}", max_rows + 1);

    let (markdown_table, columns, row_count, truncated, _shortened, rows) =
        results_to_markdown(&conn, &sql, max_rows)?;
    let _ = markdown_table;

    let mut preview = TablePreview {
        title: file_name,
        columns,
        rows,
        row_count,
        truncated,
        source: TableSource::File { path: display_path },
    };
    cap_table_preview_for_ui(&mut preview);
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn caps_columns_rows_and_cells() {
        let columns: Vec<ColumnInfo> = (0..15)
            .map(|i| ColumnInfo {
                name: format!("col{i}"),
                data_type: "VARCHAR".into(),
            })
            .collect();
        let long_cell = "x".repeat(120);
        let rows = vec![
            (0..15)
                .map(|i| format!("{long_cell}{i}"))
                .collect::<Vec<_>>(),
        ];
        let preview = table_preview_from_query("SELECT 1".into(), columns, rows, 1, false);
        assert!(preview.columns.len() <= UI_TABLE_MAX_COLS);
        assert!(preview.rows[0].len() <= UI_TABLE_MAX_COLS);
        assert!(preview.rows[0][0].chars().count() <= 83);
        assert!(preview.truncated);
    }

    #[test]
    fn load_file_preview_reads_csv() {
        let dir = tempdir().unwrap();
        let csv_path = dir.path().join("sales.csv");
        fs::write(&csv_path, "category,amount\nbook,10\ngame,30\n").unwrap();

        let preview = load_file_table_preview(
            &dir.path().to_string_lossy(),
            &csv_path.to_string_lossy(),
            10,
        )
        .unwrap();

        assert_eq!(preview.row_count, 2);
        assert_eq!(preview.column_count(), 2);
        assert!(matches!(preview.source, TableSource::File { .. }));
        assert!(preview.rows.iter().flatten().any(|c| c.contains("book")));
    }
}
