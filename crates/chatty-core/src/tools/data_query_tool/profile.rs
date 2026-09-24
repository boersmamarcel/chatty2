//! Data-profiling helpers used by `DescribeDataTool` and
//! `ProfileDataTool`.
//!
//! Pure functions that run a stable SQL query against an open DuckDB
//! connection and return the parsed result. No I/O outside the
//! connection.

use duckdb::Connection;

use super::markdown::{format_markdown_cell, results_to_markdown, value_to_string};
use super::sql::escape_sql_string;
use super::{
    ColumnInfo, ColumnProfile, DataQueryError, MAX_PROFILE_COLUMNS, MAX_PROFILE_IMPORTANT_COLUMNS,
    MAX_PROFILE_SAMPLE_COLUMNS, ProfileDataSummary, TopValue, sandboxed_connection,
};

pub(super) fn data_format_from_extension(ext: &str) -> Result<&'static str, DataQueryError> {
    match ext {
        "parquet" => Ok("parquet"),
        "csv" | "tsv" => Ok("csv"),
        "json" | "jsonl" | "ndjson" => Ok("json"),
        _ => Err(DataQueryError::UnsupportedFormat(format!(
            "Unsupported file extension '.{}'. Supported: .parquet, .csv, .tsv, .json, .jsonl, .ndjson (profile_data also reads .xlsx, .xls and .ods)",
            ext
        ))),
    }
}

/// Whether `ext` names a spreadsheet `profile_data` reads through calamine.
pub(super) fn is_spreadsheet_extension(ext: &str) -> bool {
    matches!(ext, "xlsx" | "xlsm" | "xls" | "xlsb" | "ods")
}

/// Profile a spreadsheet's first sheet: calamine reads it (DuckDB's own
/// Excel reader is an extension the sandbox cannot load), it is written as
/// CSV into a private temp dir, and that CSV is profiled like any other so
/// column types are inferred the same way. The header is the first row.
#[cfg(feature = "excel")]
pub(super) fn profile_spreadsheet(
    path: &std::path::Path,
    sample_rows: u32,
) -> Result<ProfileDataSummary, DataQueryError> {
    use calamine::{Reader, open_workbook_auto};

    let mut workbook = open_workbook_auto(path).map_err(|e| {
        DataQueryError::QueryFailed(format!("Failed to open '{}': {e}", path.display()))
    })?;
    let sheets = workbook.sheet_names().to_vec();
    let first = sheets
        .first()
        .ok_or_else(|| DataQueryError::QueryFailed("Workbook has no sheets".to_string()))?
        .clone();
    let range = workbook
        .worksheet_range(&first)
        .map_err(|e| DataQueryError::QueryFailed(format!("Failed to read sheet '{first}': {e}")))?;

    let mut csv = String::new();
    for row in range.rows() {
        let cells: Vec<String> = row.iter().map(|cell| csv_field(&cell_text(cell))).collect();
        csv.push_str(&cells.join(","));
        csv.push('\n');
    }
    let dir = tempfile::tempdir()
        .map_err(|e| DataQueryError::QueryFailed(format!("Failed to create temp dir: {e}")))?;
    let csv_path = dir.path().join("sheet.csv");
    std::fs::write(&csv_path, csv)
        .map_err(|e| DataQueryError::QueryFailed(format!("Failed to stage sheet: {e}")))?;

    let (columns, row_count, sample, profiles, mut notes) = profile_data_file(
        &dir.path().to_string_lossy(),
        &csv_path.to_string_lossy(),
        "csv",
        sample_rows,
    )?;
    notes.insert(
        0,
        format!(
            "Profiled sheet '{first}' ({} of {} sheet(s): {}); use read_excel for other sheets.",
            1,
            sheets.len(),
            sheets.join(", ")
        ),
    );
    Ok((columns, row_count, sample, profiles, notes))
}

#[cfg(not(feature = "excel"))]
pub(super) fn profile_spreadsheet(
    path: &std::path::Path,
    _sample_rows: u32,
) -> Result<ProfileDataSummary, DataQueryError> {
    Err(DataQueryError::UnsupportedFormat(format!(
        "'{}' is a spreadsheet, and this build has no Excel support",
        path.display()
    )))
}

/// A cell as CSV text. calamine's `Display` writes a date cell as its Excel
/// serial number (`45292`), which DuckDB would profile as a plain number;
/// write it as an ISO date (with the time when it has one) so it is
/// profiled as a date. Durations keep their numeric value.
#[cfg(feature = "excel")]
fn cell_text(cell: &calamine::Data) -> String {
    match cell {
        calamine::Data::DateTime(dt) if dt.is_datetime() => {
            let (y, mo, d, h, mi, s, _) = dt.to_ymd_hms_milli();
            if (h, mi, s) == (0, 0, 0) {
                format!("{y:04}-{mo:02}-{d:02}")
            } else {
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
            }
        }
        other => other.to_string(),
    }
}

/// `value` as one CSV field, quoted when it holds a separator, quote or
/// line break.
#[cfg(feature = "excel")]
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub(super) fn profile_data_file(
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
    let (sample_rows_markdown, _, _, _, shortened_values, _) =
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

pub(super) fn select_profile_columns(columns: &[ColumnInfo]) -> Vec<ColumnInfo> {
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

pub(super) fn is_likely_important_low_cardinality_column(column: &ColumnInfo) -> bool {
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

pub(super) fn describe_source(
    conn: &Connection,
    source: &str,
) -> Result<Vec<ColumnInfo>, DataQueryError> {
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

pub(super) fn count_source_rows(conn: &Connection, source: &str) -> Result<u64, DataQueryError> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {source}"), [], |row| {
        row.get(0)
    })
    .map_err(|e| DataQueryError::QueryFailed(e.to_string()))
}

pub(super) fn profile_column(
    conn: &Connection,
    source: &str,
    column: &ColumnInfo,
) -> ColumnProfile {
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

pub(super) fn top_values_for_column(
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

pub(super) fn data_source_sql(format: &str, escaped_path: &str) -> String {
    match format {
        "parquet" => format!("read_parquet('{escaped_path}')"),
        "csv" => format!("read_csv('{escaped_path}')"),
        "json" => format!("read_json_auto('{escaped_path}')"),
        _ => unreachable!(),
    }
}

pub(super) fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub(super) fn is_numeric_type(data_type: &str) -> bool {
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

pub(super) fn should_collect_top_values(data_type: &str) -> bool {
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

pub(super) fn is_complex_type(upper_data_type: &str) -> bool {
    upper_data_type.contains("[]")
        || upper_data_type.contains("LIST")
        || upper_data_type.contains("STRUCT")
        || upper_data_type.contains("MAP")
}

#[cfg(all(test, feature = "excel"))]
mod spreadsheet_tests {
    use super::cell_text;
    use calamine::{Data, ExcelDateTime, ExcelDateTimeType};

    #[test]
    fn date_cells_are_written_as_iso_dates_not_serials() {
        let date = Data::DateTime(ExcelDateTime::new(
            45292.0,
            ExcelDateTimeType::DateTime,
            false,
        ));
        assert_eq!(cell_text(&date), "2024-01-01");
        let noon = Data::DateTime(ExcelDateTime::new(
            45292.5,
            ExcelDateTimeType::DateTime,
            false,
        ));
        assert_eq!(cell_text(&noon), "2024-01-01 12:00:00");
        assert_eq!(cell_text(&Data::Float(1.5)), "1.5");
    }
}
