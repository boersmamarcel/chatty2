use calamine::{Data, Reader, open_workbook_auto};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use rust_xlsxwriter::{Format, FormatBorder, Workbook};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tracing::warn;

use crate::chatty::models::write_approval_store::{PendingWriteApprovals, WriteOperation};
use crate::chatty::services::filesystem_service::FileSystemService;

// Re-use the write approval helper from filesystem_write_tool
use super::filesystem_write_tool::request_write_approval;

// ─── helpers ───

/// Convert column letter(s) to 0-based index (A=0, B=1, ..., Z=25, AA=26, ...).
fn col_from_letters(s: &str) -> u16 {
    let mut col: u16 = 0;
    for b in s.bytes() {
        col = col * 26 + (b - b'A') as u16 + 1;
    }
    col - 1
}

/// Parse a cell reference like "A1" or "AB123" into (row, col) 0-based indices.
fn parse_cell_ref(s: &str) -> Result<(u32, u16), ExcelToolError> {
    let s = s.trim().to_uppercase();
    let split = s
        .find(|c: char| c.is_ascii_digit())
        .ok_or_else(|| ExcelToolError::InvalidCellRef(s.clone()))?;
    let (letters, digits) = s.split_at(split);
    if letters.is_empty() || digits.is_empty() {
        return Err(ExcelToolError::InvalidCellRef(s.clone()));
    }
    let col = col_from_letters(letters);
    let row: u32 = digits
        .parse::<u32>()
        .map_err(|_| ExcelToolError::InvalidCellRef(s.clone()))?
        .checked_sub(1)
        .ok_or_else(|| ExcelToolError::InvalidCellRef(s.clone()))?;
    Ok((row, col))
}

/// A cell coordinate (row, col) in 0-based indices.
type CellCoord = (u32, u16);

/// Parse a range like "A1:D10" into ((start_row, start_col), (end_row, end_col)).
fn parse_range(s: &str) -> Result<(CellCoord, CellCoord), ExcelToolError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(ExcelToolError::InvalidRange(s.to_string()));
    }
    let start = parse_cell_ref(parts[0])?;
    let end = parse_cell_ref(parts[1])?;
    Ok((start, end))
}

/// Convert a calamine `Data` cell to a JSON value.
fn calamine_to_json(cell: &Data) -> Value {
    match cell {
        Data::Int(n) => Value::Number((*n).into()),
        Data::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Data::String(s) => Value::String(s.clone()),
        Data::Bool(b) => Value::Bool(*b),
        Data::DateTime(dt) => Value::String(format!("{}", dt.as_f64())),
        Data::DateTimeIso(s) => Value::String(s.clone()),
        Data::DurationIso(s) => Value::String(s.clone()),
        Data::Error(e) => Value::String(format!("#ERR:{:?}", e)),
        Data::Empty => Value::Null,
    }
}

/// Format rows as a markdown table string. Shows at most `max_rows` data rows.
fn format_markdown_table(rows: &[Vec<Value>], max_rows: usize) -> String {
    if rows.is_empty() {
        return String::from("(empty sheet)");
    }

    let display_rows = rows.len().min(max_rows + 1); // +1 for potential header
    let num_cols = rows
        .iter()
        .take(display_rows)
        .map(|r| r.len())
        .max()
        .unwrap_or(0);
    if num_cols == 0 {
        return String::from("(empty sheet)");
    }

    // Compute column widths
    let mut widths = vec![3usize; num_cols];
    for row in rows.iter().take(display_rows) {
        for (i, val) in row.iter().enumerate() {
            let s = cell_display(val);
            widths[i] = widths[i].max(s.len());
        }
    }

    let mut out = String::new();

    // Header row
    if let Some(header) = rows.first() {
        out.push('|');
        for (i, val) in header.iter().enumerate() {
            let s = cell_display(val);
            out.push_str(&format!(" {:width$} |", s, width = widths[i]));
        }
        // Pad missing columns
        for w in widths.iter().take(num_cols).skip(header.len()) {
            out.push_str(&format!(" {:width$} |", "", width = w));
        }
        out.push('\n');

        // Separator
        out.push('|');
        for w in &widths {
            out.push_str(&format!("-{}-|", "-".repeat(*w)));
        }
        out.push('\n');
    }

    // Data rows
    let data_start = 1;
    let data_end = display_rows.min(rows.len());
    for row in rows.iter().take(data_end).skip(data_start) {
        out.push('|');
        for (i, val) in row.iter().enumerate() {
            let s = cell_display(val);
            out.push_str(&format!(" {:width$} |", s, width = widths[i]));
        }
        for w in widths.iter().take(num_cols).skip(row.len()) {
            out.push_str(&format!(" {:width$} |", "", width = w));
        }
        out.push('\n');
    }

    if rows.len() > display_rows {
        out.push_str(&format!(
            "... and {} more rows\n",
            rows.len() - display_rows
        ));
    }

    out
}

fn cell_display(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse a hex color string like "FF0000" or "#FF0000" into a u32 RGB value.
fn parse_hex_color(s: &str) -> Result<u32, ExcelToolError> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    u32::from_str_radix(hex, 16).map_err(|_| ExcelToolError::InvalidColor(s.to_string()))
}

/// Write a JSON value to a worksheet cell.
fn write_cell_value(
    sheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    col: u16,
    val: &Value,
    fmt: Option<&Format>,
) -> Result<(), ExcelToolError> {
    match val {
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if let Some(fmt) = fmt {
                sheet.write_number_with_format(row, col, f, fmt)
            } else {
                sheet.write_number(row, col, f)
            }
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
        }
        Value::Bool(b) => {
            if let Some(fmt) = fmt {
                sheet.write_boolean_with_format(row, col, *b, fmt)
            } else {
                sheet.write_boolean(row, col, *b)
            }
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
        }
        Value::Null => {} // leave cell empty
        _ => {
            let s = match val {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if let Some(fmt) = fmt {
                sheet.write_string_with_format(row, col, &s, fmt)
            } else {
                sheet.write_string(row, col, &s)
            }
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
        }
    }
    Ok(())
}

// ─── error type ───

#[derive(Debug, thiserror::Error)]
pub enum ExcelToolError {
    #[error("Excel error: {0}")]
    OperationError(#[from] anyhow::Error),
    #[error("Invalid cell reference: {0}")]
    InvalidCellRef(String),
    #[error("Invalid range: {0}")]
    InvalidRange(String),
    #[error("Invalid color: {0}")]
    InvalidColor(String),
    #[error("Write error: {0}")]
    WriteError(String),
}

// ═══════════════════════════════════════════════════════════════════════
// read_excel tool
// ═══════════════════════════════════════════════════════════════════════

#[derive(Deserialize, Serialize)]
pub struct ReadExcelArgs {
    pub path: String,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub range: Option<String>,
    #[serde(default)]
    pub max_rows: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct ReadExcelOutput {
    pub path: String,
    pub sheet_names: Vec<String>,
    pub selected_sheet: String,
    pub dimensions: String,
    pub total_rows: usize,
    pub rows_returned: usize,
    pub data: Vec<Vec<Value>>,
    pub markdown_preview: String,
}

#[derive(Clone)]
pub struct ReadExcelTool {
    service: Arc<FileSystemService>,
}

impl ReadExcelTool {
    pub fn new(service: Arc<FileSystemService>) -> Self {
        Self { service }
    }
}

impl Tool for ReadExcelTool {
    const NAME: &'static str = "read_excel";
    type Error = ExcelToolError;
    type Args = ReadExcelArgs;
    type Output = ReadExcelOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "read_excel".to_string(),
            description: "Read an Excel spreadsheet file and return its contents as structured data.\n\
                         Returns sheet names, cell data as JSON arrays, and a markdown table preview.\n\
                         \n\
                         Supported formats: .xlsx, .xls, .xlsm, .xlsb, .ods\n\
                         \n\
                         Examples:\n\
                         - Read first sheet: {\"path\": \"data/report.xlsx\"}\n\
                         - Read specific sheet: {\"path\": \"data/report.xlsx\", \"sheet\": \"Sales\"}\n\
                         - Read a range: {\"path\": \"data/report.xlsx\", \"range\": \"A1:D50\"}\n\
                         - Limit rows: {\"path\": \"data/report.xlsx\", \"max_rows\": 100}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the Excel file, relative to workspace root or absolute within workspace"
                    },
                    "sheet": {
                        "type": "string",
                        "description": "Sheet name to read. Defaults to the first sheet."
                    },
                    "range": {
                        "type": "string",
                        "description": "Cell range to read, e.g. 'A1:D50'. Defaults to all used cells."
                    },
                    "max_rows": {
                        "type": "integer",
                        "description": "Maximum number of rows to return. Defaults to 500."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;
        let max_rows = args.max_rows.unwrap_or(500);

        // Open the workbook (calamine auto-detects format)
        let mut workbook = open_workbook_auto(&canonical).map_err(|e| {
            ExcelToolError::OperationError(anyhow::anyhow!("Failed to open '{}': {}", args.path, e))
        })?;

        let sheet_names: Vec<String> = workbook.sheet_names().to_vec();
        if sheet_names.is_empty() {
            return Err(ExcelToolError::OperationError(anyhow::anyhow!(
                "Workbook has no sheets"
            )));
        }

        // Select sheet
        let selected_sheet = args.sheet.unwrap_or_else(|| sheet_names[0].clone());
        let range_data = workbook.worksheet_range(&selected_sheet).map_err(|e| {
            ExcelToolError::OperationError(anyhow::anyhow!(
                "Failed to read sheet '{}': {}",
                selected_sheet,
                e
            ))
        })?;

        let total_rows = range_data.height();
        let total_cols = range_data.width();

        // Convert to JSON rows, optionally filtering by range
        let mut rows: Vec<Vec<Value>> = Vec::new();

        if let Some(ref range_str) = args.range {
            let ((sr, sc), (er, ec)) = parse_range(range_str)?;
            for r in sr..=er {
                let mut row_data = Vec::new();
                for c in sc..=ec {
                    let val = range_data
                        .get((r as usize, c as usize))
                        .map(calamine_to_json)
                        .unwrap_or(Value::Null);
                    row_data.push(val);
                }
                rows.push(row_data);
            }
        } else {
            for r in 0..total_rows {
                let mut row_data = Vec::new();
                for c in 0..total_cols {
                    let val = range_data
                        .get((r, c))
                        .map(calamine_to_json)
                        .unwrap_or(Value::Null);
                    row_data.push(val);
                }
                rows.push(row_data);
            }
        }

        // Cap to max_rows (keep header + max_rows data rows)
        let rows_returned = rows.len();
        if rows.len() > max_rows + 1 {
            rows.truncate(max_rows + 1);
        }
        let rows_returned_final = rows.len();

        let markdown_preview = format_markdown_table(&rows, 20);
        let dimensions = format!("{}R x {}C", total_rows, total_cols);

        Ok(ReadExcelOutput {
            path: canonical.display().to_string(),
            sheet_names,
            selected_sheet,
            dimensions,
            total_rows: rows_returned,
            rows_returned: rows_returned_final,
            data: rows,
            markdown_preview,
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════
// write_excel tool
// ═══════════════════════════════════════════════════════════════════════

#[derive(Deserialize, Serialize)]
pub struct WriteExcelArgs {
    pub path: String,
    pub sheets: Vec<SheetSpec>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct SheetSpec {
    pub name: String,
    #[serde(default)]
    pub data: Vec<Vec<Value>>,
    #[serde(default)]
    pub column_widths: Vec<f64>,
    #[serde(default)]
    pub formatting: Option<SheetFormatting>,
    #[serde(default)]
    pub cell_formats: Vec<CellFormatSpec>,
    #[serde(default)]
    pub merged_cells: Vec<String>,
    #[serde(default)]
    pub formulas: Vec<FormulaSpec>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct SheetFormatting {
    #[serde(default = "default_true")]
    pub header_bold: bool,
    #[serde(default)]
    pub auto_filter: bool,
    #[serde(default)]
    pub freeze_top_row: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize, Serialize, Clone)]
pub struct CellFormatSpec {
    pub range: String,
    #[serde(default)]
    pub bold: Option<bool>,
    #[serde(default)]
    pub italic: Option<bool>,
    #[serde(default)]
    pub font_color: Option<String>,
    #[serde(default)]
    pub bg_color: Option<String>,
    #[serde(default)]
    pub number_format: Option<String>,
    #[serde(default)]
    pub border: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct FormulaSpec {
    pub cell: String,
    pub formula: String,
}

#[derive(Debug, Serialize)]
pub struct WriteExcelOutput {
    pub path: String,
    pub sheets_written: usize,
    pub total_rows: usize,
    pub message: String,
}

#[derive(Clone)]
pub struct WriteExcelTool {
    service: Arc<FileSystemService>,
    pending_approvals: PendingWriteApprovals,
}

impl WriteExcelTool {
    pub fn new(service: Arc<FileSystemService>, pending_approvals: PendingWriteApprovals) -> Self {
        Self {
            service,
            pending_approvals,
        }
    }
}

/// Build a `rust_xlsxwriter::Format` from a `CellFormatSpec`.
fn build_format(spec: &CellFormatSpec) -> Result<Format, ExcelToolError> {
    let mut fmt = Format::new();
    if spec.bold == Some(true) {
        fmt = fmt.set_bold();
    }
    if spec.italic == Some(true) {
        fmt = fmt.set_italic();
    }
    if let Some(ref color) = spec.font_color {
        let rgb = parse_hex_color(color)?;
        fmt = fmt.set_font_color(rgb);
    }
    if let Some(ref color) = spec.bg_color {
        let rgb = parse_hex_color(color)?;
        fmt = fmt.set_background_color(rgb);
    }
    if let Some(ref nf) = spec.number_format {
        fmt = fmt.set_num_format(nf);
    }
    if let Some(ref border) = spec.border {
        let b = match border.as_str() {
            "thin" => FormatBorder::Thin,
            "medium" => FormatBorder::Medium,
            "thick" => FormatBorder::Thick,
            _ => FormatBorder::Thin,
        };
        fmt = fmt.set_border(b);
    }
    Ok(fmt)
}

/// Write one `SheetSpec` to a workbook. Returns the number of data rows written.
fn write_sheet(workbook: &mut Workbook, spec: &SheetSpec) -> Result<usize, ExcelToolError> {
    let sheet = workbook
        .add_worksheet()
        .set_name(&spec.name)
        .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;

    let formatting = spec.formatting.clone().unwrap_or(SheetFormatting {
        header_bold: true,
        auto_filter: false,
        freeze_top_row: false,
    });

    // Write data
    let header_fmt = if formatting.header_bold {
        Some(Format::new().set_bold())
    } else {
        None
    };

    let mut total_rows = 0;
    for (r, row) in spec.data.iter().enumerate() {
        for (c, val) in row.iter().enumerate() {
            let fmt = if r == 0 { header_fmt.as_ref() } else { None };
            write_cell_value(sheet, r as u32, c as u16, val, fmt)?;
        }
        total_rows += 1;
    }

    // Column widths
    for (i, &w) in spec.column_widths.iter().enumerate() {
        sheet
            .set_column_width(i as u16, w)
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
    }

    // Auto-filter
    if formatting.auto_filter && !spec.data.is_empty() {
        let last_col = spec
            .data
            .iter()
            .map(|r| r.len())
            .max()
            .unwrap_or(1)
            .saturating_sub(1);
        let last_row = spec.data.len().saturating_sub(1);
        sheet
            .autofilter(0, 0, last_row as u32, last_col as u16)
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
    }

    // Freeze top row
    if formatting.freeze_top_row {
        sheet
            .set_freeze_panes(1, 0)
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
    }

    // Cell formats
    for cf in &spec.cell_formats {
        let fmt = build_format(cf)?;
        let ((sr, sc), (er, ec)) = parse_range(&cf.range)?;
        for r in sr..=er {
            for c in sc..=ec {
                // Re-write existing cell value with format, or write empty with format
                let existing = spec
                    .data
                    .get(r as usize)
                    .and_then(|row| row.get(c as usize))
                    .cloned()
                    .unwrap_or(Value::Null);
                write_cell_value(sheet, r, c, &existing, Some(&fmt))?;
            }
        }
    }

    // Merged cells
    for merge_range in &spec.merged_cells {
        let ((sr, sc), (er, ec)) = parse_range(merge_range)?;
        sheet
            .merge_range(sr, sc, er, ec, "", &Format::new())
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
    }

    // Formulas
    for f in &spec.formulas {
        let (r, c) = parse_cell_ref(&f.cell)?;
        sheet
            .write_formula(r, c, f.formula.as_str())
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
    }

    Ok(total_rows)
}

impl Tool for WriteExcelTool {
    const NAME: &'static str = "write_excel";
    type Error = ExcelToolError;
    type Args = WriteExcelArgs;
    type Output = WriteExcelOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "write_excel".to_string(),
            description: "Create a new Excel (.xlsx) file with structured data and formatting.\n\
                         Supports multiple sheets, bold/italic/color formatting, column widths,\n\
                         auto-filters, frozen panes, merged cells, number formats, borders, and formulas.\n\
                         \n\
                         Requires user approval before writing.\n\
                         \n\
                         Examples:\n\
                         - Simple data: {\"path\": \"output.xlsx\", \"sheets\": [{\"name\": \"Sheet1\", \"data\": [[\"Name\",\"Age\"],[\"Alice\",30]]}]}\n\
                         - With formatting: {\"path\": \"report.xlsx\", \"sheets\": [{\"name\": \"Sales\", \"data\": [...], \"formatting\": {\"header_bold\": true, \"auto_filter\": true, \"freeze_top_row\": true}, \"column_widths\": [20, 15, 12]}]}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Output path for the .xlsx file, relative to workspace root"
                    },
                    "sheets": {
                        "type": "array",
                        "description": "Array of sheet specifications",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {
                                    "type": "string",
                                    "description": "Sheet name"
                                },
                                "data": {
                                    "type": "array",
                                    "description": "Array of rows, each row is an array of cell values (string, number, boolean, or null)",
                                    "items": {
                                        "type": "array",
                                        "items": {
                                            "oneOf": [
                                                { "type": "string" },
                                                { "type": "number" },
                                                { "type": "boolean" },
                                                { "type": "null" }
                                            ]
                                        }
                                    }
                                },
                                "column_widths": {
                                    "type": "array",
                                    "description": "Column widths in character units",
                                    "items": { "type": "number" }
                                },
                                "formatting": {
                                    "type": "object",
                                    "properties": {
                                        "header_bold": { "type": "boolean", "description": "Bold the first row (default: true)" },
                                        "auto_filter": { "type": "boolean", "description": "Add auto-filter to header row" },
                                        "freeze_top_row": { "type": "boolean", "description": "Freeze the top row" }
                                    }
                                },
                                "cell_formats": {
                                    "type": "array",
                                    "description": "Cell formatting specifications",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "range": { "type": "string", "description": "Cell range, e.g. 'A1:D1'" },
                                            "bold": { "type": "boolean" },
                                            "italic": { "type": "boolean" },
                                            "font_color": { "type": "string", "description": "Hex color, e.g. 'FF0000'" },
                                            "bg_color": { "type": "string", "description": "Hex color, e.g. 'FFFF00'" },
                                            "number_format": { "type": "string", "description": "Number format, e.g. '#,##0.00'" },
                                            "border": { "type": "string", "description": "'thin', 'medium', or 'thick'" }
                                        },
                                        "required": ["range"]
                                    }
                                },
                                "merged_cells": {
                                    "type": "array",
                                    "description": "Cell ranges to merge, e.g. ['A1:D1']",
                                    "items": { "type": "string" }
                                },
                                "formulas": {
                                    "type": "array",
                                    "description": "Cell formulas",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "cell": { "type": "string", "description": "Cell reference, e.g. 'E2'" },
                                            "formula": { "type": "string", "description": "Excel formula, e.g. '=SUM(A2:D2)'" }
                                        },
                                        "required": ["cell", "formula"]
                                    }
                                }
                            },
                            "required": ["name"]
                        }
                    }
                },
                "required": ["path", "sheets"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;
        let is_overwrite = canonical.exists();

        // Request approval
        let approved = request_write_approval(
            &self.pending_approvals,
            WriteOperation::WriteFile {
                path: canonical.display().to_string(),
                is_overwrite,
                content_preview: format!("Excel workbook with {} sheet(s)", args.sheets.len()),
            },
        )
        .await?;

        if !approved {
            return Err(ExcelToolError::OperationError(anyhow::anyhow!(
                "Write denied by user"
            )));
        }

        let mut workbook = Workbook::new();
        let mut total_rows = 0;
        let sheets_count = args.sheets.len();

        for spec in &args.sheets {
            total_rows += write_sheet(&mut workbook, spec)?;
        }

        workbook
            .save(&canonical)
            .map_err(|e| ExcelToolError::WriteError(format!("Failed to save: {}", e)))?;

        Ok(WriteExcelOutput {
            path: canonical.display().to_string(),
            sheets_written: sheets_count,
            total_rows,
            message: format!(
                "Created Excel file '{}' with {} sheet(s) and {} total rows.",
                args.path, sheets_count, total_rows
            ),
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════
// edit_excel tool
// ═══════════════════════════════════════════════════════════════════════

#[derive(Deserialize, Serialize)]
pub struct EditExcelArgs {
    pub path: String,
    #[serde(default)]
    pub output_path: Option<String>,
    pub operations: Vec<EditOperation>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EditOperation {
    SetCell {
        sheet: String,
        cell: String,
        value: Value,
    },
    SetRange {
        sheet: String,
        start_cell: String,
        data: Vec<Vec<Value>>,
    },
    AddSheet {
        name: String,
        #[serde(default)]
        data: Vec<Vec<Value>>,
    },
    DeleteRows {
        sheet: String,
        start_row: usize,
        count: usize,
    },
    SetFormula {
        sheet: String,
        cell: String,
        formula: String,
    },
    FormatRange {
        sheet: String,
        range: String,
        #[serde(default)]
        bold: Option<bool>,
        #[serde(default)]
        italic: Option<bool>,
        #[serde(default)]
        font_color: Option<String>,
        #[serde(default)]
        bg_color: Option<String>,
        #[serde(default)]
        number_format: Option<String>,
        #[serde(default)]
        border: Option<String>,
    },
}

#[derive(Debug, Serialize)]
pub struct EditExcelOutput {
    pub path: String,
    pub operations_applied: usize,
    pub sheets: Vec<String>,
    pub message: String,
    pub warning: Option<String>,
}

#[derive(Clone)]
pub struct EditExcelTool {
    service: Arc<FileSystemService>,
    pending_approvals: PendingWriteApprovals,
}

impl EditExcelTool {
    pub fn new(service: Arc<FileSystemService>, pending_approvals: PendingWriteApprovals) -> Self {
        Self {
            service,
            pending_approvals,
        }
    }
}

/// Internal representation of sheet data for editing.
struct SheetData {
    name: String,
    rows: Vec<Vec<Value>>,
    formulas: Vec<(u32, u16, String)>,
    formats: Vec<(String, CellFormatSpec)>,
}

impl Tool for EditExcelTool {
    const NAME: &'static str = "edit_excel";
    type Error = ExcelToolError;
    type Args = EditExcelArgs;
    type Output = EditExcelOutput;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "edit_excel".to_string(),
            description: "Edit an existing Excel file by applying targeted modifications.\n\
                         Reads the file, applies operations, and writes back.\n\
                         \n\
                         WARNING: This tool reads data with calamine and rewrites with rust_xlsxwriter.\n\
                         Original formatting, macros, charts, images, and VBA code will be LOST.\n\
                         Only cell values and structure are preserved. Use 'output_path' to save\n\
                         to a different file if you want to keep the original.\n\
                         \n\
                         Supported operations: set_cell, set_range, add_sheet, delete_rows, set_formula, format_range.\n\
                         \n\
                         Examples:\n\
                         - Update a cell: {\"path\": \"data.xlsx\", \"operations\": [{\"type\": \"set_cell\", \"sheet\": \"Sheet1\", \"cell\": \"B3\", \"value\": 42}]}\n\
                         - Add a row of data: {\"path\": \"data.xlsx\", \"operations\": [{\"type\": \"set_range\", \"sheet\": \"Sheet1\", \"start_cell\": \"A10\", \"data\": [[\"New\", \"Row\", 123]]}]}\n\
                         - Save to new file: {\"path\": \"data.xlsx\", \"output_path\": \"data_modified.xlsx\", \"operations\": [...]}"
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the existing Excel file"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "Optional output path. Defaults to overwriting the input file."
                    },
                    "operations": {
                        "type": "array",
                        "description": "List of edit operations to apply",
                        "items": {
                            "type": "object",
                            "properties": {
                                "type": {
                                    "type": "string",
                                    "enum": ["set_cell", "set_range", "add_sheet", "delete_rows", "set_formula", "format_range"],
                                    "description": "Operation type"
                                },
                                "sheet": { "type": "string", "description": "Target sheet name" },
                                "cell": { "type": "string", "description": "Cell reference (for set_cell, set_formula)" },
                                "value": { "description": "Cell value (for set_cell)" },
                                "start_cell": { "type": "string", "description": "Starting cell for set_range" },
                                "data": {
                                    "type": "array",
                                    "description": "Row data (for set_range, add_sheet). Each element is an array of cell values.",
                                    "items": {
                                        "type": "array",
                                        "items": {
                                            "oneOf": [
                                                { "type": "string" },
                                                { "type": "number" },
                                                { "type": "boolean" },
                                                { "type": "null" }
                                            ]
                                        }
                                    }
                                },
                                "name": { "type": "string", "description": "Sheet name (for add_sheet)" },
                                "start_row": { "type": "integer", "description": "1-based start row (for delete_rows)" },
                                "count": { "type": "integer", "description": "Number of rows to delete" },
                                "formula": { "type": "string", "description": "Excel formula (for set_formula)" },
                                "range": { "type": "string", "description": "Cell range (for format_range)" },
                                "bold": { "type": "boolean" },
                                "italic": { "type": "boolean" },
                                "font_color": { "type": "string" },
                                "bg_color": { "type": "string" },
                                "number_format": { "type": "string" },
                                "border": { "type": "string" }
                            },
                            "required": ["type"]
                        }
                    }
                },
                "required": ["path", "operations"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let canonical = self.service.resolve_path(&args.path).await?;
        let output_canonical = if let Some(ref out) = args.output_path {
            self.service.resolve_path(out).await?
        } else {
            canonical.clone()
        };

        // Request approval
        let op_summary = args
            .operations
            .iter()
            .map(|op| match op {
                EditOperation::SetCell { sheet, cell, .. } => {
                    format!("set_cell({}.{})", sheet, cell)
                }
                EditOperation::SetRange {
                    sheet,
                    start_cell,
                    data,
                    ..
                } => {
                    format!("set_range({}.{}, {} rows)", sheet, start_cell, data.len())
                }
                EditOperation::AddSheet { name, .. } => format!("add_sheet({})", name),
                EditOperation::DeleteRows {
                    sheet,
                    start_row,
                    count,
                    ..
                } => {
                    format!("delete_rows({}, row {} x{})", sheet, start_row, count)
                }
                EditOperation::SetFormula { sheet, cell, .. } => {
                    format!("set_formula({}.{})", sheet, cell)
                }
                EditOperation::FormatRange { sheet, range, .. } => {
                    format!("format_range({}.{})", sheet, range)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");

        let approved = request_write_approval(
            &self.pending_approvals,
            WriteOperation::WriteFile {
                path: output_canonical.display().to_string(),
                is_overwrite: output_canonical.exists(),
                content_preview: format!(
                    "Edit Excel: {} operation(s) [{}]",
                    args.operations.len(),
                    if op_summary.len() > 150 {
                        format!("{}...", &op_summary[..150])
                    } else {
                        op_summary.clone()
                    }
                ),
            },
        )
        .await?;

        if !approved {
            return Err(ExcelToolError::OperationError(anyhow::anyhow!(
                "Edit denied by user"
            )));
        }

        // PHASE 1: Read existing data with calamine
        let mut workbook = open_workbook_auto(&canonical).map_err(|e| {
            ExcelToolError::OperationError(anyhow::anyhow!("Failed to open '{}': {}", args.path, e))
        })?;

        let sheet_names: Vec<String> = workbook.sheet_names().to_vec();
        let mut sheets: Vec<SheetData> = Vec::new();

        for name in &sheet_names {
            let range = workbook.worksheet_range(name).map_err(|e| {
                ExcelToolError::OperationError(anyhow::anyhow!(
                    "Failed to read sheet '{}': {}",
                    name,
                    e
                ))
            })?;

            let mut rows = Vec::new();
            for r in 0..range.height() {
                let mut row_data = Vec::new();
                for c in 0..range.width() {
                    let val = range
                        .get((r, c))
                        .map(calamine_to_json)
                        .unwrap_or(Value::Null);
                    row_data.push(val);
                }
                rows.push(row_data);
            }

            sheets.push(SheetData {
                name: name.clone(),
                rows,
                formulas: Vec::new(),
                formats: Vec::new(),
            });
        }

        // PHASE 2: Apply operations
        let ops_count = args.operations.len();
        for op in &args.operations {
            match op {
                EditOperation::SetCell { sheet, cell, value } => {
                    let (r, c) = parse_cell_ref(cell)?;
                    let sd = sheets
                        .iter_mut()
                        .find(|s| s.name == *sheet)
                        .ok_or_else(|| {
                            ExcelToolError::OperationError(anyhow::anyhow!(
                                "Sheet '{}' not found",
                                sheet
                            ))
                        })?;
                    // Expand rows/cols if needed
                    while sd.rows.len() <= r as usize {
                        sd.rows.push(Vec::new());
                    }
                    let row = &mut sd.rows[r as usize];
                    while row.len() <= c as usize {
                        row.push(Value::Null);
                    }
                    row[c as usize] = value.clone();
                }
                EditOperation::SetRange {
                    sheet,
                    start_cell,
                    data,
                } => {
                    let (sr, sc) = parse_cell_ref(start_cell)?;
                    let sd = sheets
                        .iter_mut()
                        .find(|s| s.name == *sheet)
                        .ok_or_else(|| {
                            ExcelToolError::OperationError(anyhow::anyhow!(
                                "Sheet '{}' not found",
                                sheet
                            ))
                        })?;
                    for (ri, row_data) in data.iter().enumerate() {
                        let target_row = sr as usize + ri;
                        while sd.rows.len() <= target_row {
                            sd.rows.push(Vec::new());
                        }
                        let row = &mut sd.rows[target_row];
                        for (ci, val) in row_data.iter().enumerate() {
                            let target_col = sc as usize + ci;
                            while row.len() <= target_col {
                                row.push(Value::Null);
                            }
                            row[target_col] = val.clone();
                        }
                    }
                }
                EditOperation::AddSheet { name, data } => {
                    if sheets.iter().any(|s| s.name == *name) {
                        warn!(sheet = %name, "Sheet already exists, skipping add_sheet");
                        continue;
                    }
                    sheets.push(SheetData {
                        name: name.clone(),
                        rows: data.clone(),
                        formulas: Vec::new(),
                        formats: Vec::new(),
                    });
                }
                EditOperation::DeleteRows {
                    sheet,
                    start_row,
                    count,
                } => {
                    let sd = sheets
                        .iter_mut()
                        .find(|s| s.name == *sheet)
                        .ok_or_else(|| {
                            ExcelToolError::OperationError(anyhow::anyhow!(
                                "Sheet '{}' not found",
                                sheet
                            ))
                        })?;
                    // start_row is 1-based
                    let idx = start_row.saturating_sub(1);
                    let end = (idx + count).min(sd.rows.len());
                    if idx < sd.rows.len() {
                        sd.rows.drain(idx..end);
                    }
                }
                EditOperation::SetFormula {
                    sheet,
                    cell,
                    formula,
                } => {
                    let (r, c) = parse_cell_ref(cell)?;
                    let sd = sheets
                        .iter_mut()
                        .find(|s| s.name == *sheet)
                        .ok_or_else(|| {
                            ExcelToolError::OperationError(anyhow::anyhow!(
                                "Sheet '{}' not found",
                                sheet
                            ))
                        })?;
                    sd.formulas.push((r, c, formula.clone()));
                }
                EditOperation::FormatRange {
                    sheet,
                    range,
                    bold,
                    italic,
                    font_color,
                    bg_color,
                    number_format,
                    border,
                } => {
                    let sd = sheets
                        .iter_mut()
                        .find(|s| s.name == *sheet)
                        .ok_or_else(|| {
                            ExcelToolError::OperationError(anyhow::anyhow!(
                                "Sheet '{}' not found",
                                sheet
                            ))
                        })?;
                    sd.formats.push((
                        range.clone(),
                        CellFormatSpec {
                            range: range.clone(),
                            bold: *bold,
                            italic: *italic,
                            font_color: font_color.clone(),
                            bg_color: bg_color.clone(),
                            number_format: number_format.clone(),
                            border: border.clone(),
                        },
                    ));
                }
            }
        }

        // PHASE 3: Write back with rust_xlsxwriter
        let mut wb = Workbook::new();
        let final_sheet_names: Vec<String> = sheets.iter().map(|s| s.name.clone()).collect();

        for sd in &sheets {
            let ws = wb
                .add_worksheet()
                .set_name(&sd.name)
                .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;

            // Write cell data
            for (r, row) in sd.rows.iter().enumerate() {
                for (c, val) in row.iter().enumerate() {
                    write_cell_value(ws, r as u32, c as u16, val, None)?;
                }
            }

            // Write formulas
            for (r, c, formula) in &sd.formulas {
                ws.write_formula(*r, *c, formula.as_str())
                    .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
            }

            // Apply formats
            for (_range_str, spec) in &sd.formats {
                let fmt = build_format(spec)?;
                let ((sr, sc), (er, ec)) = parse_range(&spec.range)?;
                for r in sr..=er {
                    for c in sc..=ec {
                        let existing = sd
                            .rows
                            .get(r as usize)
                            .and_then(|row| row.get(c as usize))
                            .cloned()
                            .unwrap_or(Value::Null);
                        write_cell_value(ws, r, c, &existing, Some(&fmt))?;
                    }
                }
            }
        }

        wb.save(&output_canonical)
            .map_err(|e| ExcelToolError::WriteError(format!("Failed to save: {}", e)))?;

        Ok(EditExcelOutput {
            path: output_canonical.display().to_string(),
            operations_applied: ops_count,
            sheets: final_sheet_names,
            message: format!(
                "Applied {} operation(s) to '{}' and saved.",
                ops_count,
                output_canonical.display()
            ),
            warning: Some(
                "Original formatting, macros, charts, and images may have been lost during the read-modify-write cycle."
                    .to_string(),
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cell_ref() {
        assert_eq!(parse_cell_ref("A1").unwrap(), (0, 0));
        assert_eq!(parse_cell_ref("B3").unwrap(), (2, 1));
        assert_eq!(parse_cell_ref("Z1").unwrap(), (0, 25));
        assert_eq!(parse_cell_ref("AA1").unwrap(), (0, 26));
        assert_eq!(parse_cell_ref("AB10").unwrap(), (9, 27));
    }

    #[test]
    fn test_parse_cell_ref_case_insensitive() {
        assert_eq!(parse_cell_ref("a1").unwrap(), (0, 0));
        assert_eq!(parse_cell_ref("ab10").unwrap(), (9, 27));
    }

    #[test]
    fn test_parse_cell_ref_invalid() {
        assert!(parse_cell_ref("").is_err());
        assert!(parse_cell_ref("123").is_err());
        assert!(parse_cell_ref("A0").is_err());
        assert!(parse_cell_ref("A").is_err());
    }

    #[test]
    fn test_parse_range() {
        let ((sr, sc), (er, ec)) = parse_range("A1:D10").unwrap();
        assert_eq!((sr, sc), (0, 0));
        assert_eq!((er, ec), (9, 3));
    }

    #[test]
    fn test_parse_range_invalid() {
        assert!(parse_range("A1").is_err());
        assert!(parse_range("A1:B2:C3").is_err());
    }

    #[test]
    fn test_col_from_letters() {
        assert_eq!(col_from_letters("A"), 0);
        assert_eq!(col_from_letters("B"), 1);
        assert_eq!(col_from_letters("Z"), 25);
        assert_eq!(col_from_letters("AA"), 26);
        assert_eq!(col_from_letters("AB"), 27);
        assert_eq!(col_from_letters("AZ"), 51);
    }

    #[test]
    fn test_parse_hex_color() {
        assert_eq!(parse_hex_color("FF0000").unwrap(), 0xFF0000);
        assert_eq!(parse_hex_color("#00FF00").unwrap(), 0x00FF00);
        assert!(parse_hex_color("GGGGGG").is_err());
    }

    #[test]
    fn test_calamine_to_json() {
        assert_eq!(calamine_to_json(&Data::Int(42)), Value::Number(42.into()));
        assert_eq!(
            calamine_to_json(&Data::String("hello".to_string())),
            Value::String("hello".to_string())
        );
        assert_eq!(calamine_to_json(&Data::Bool(true)), Value::Bool(true));
        assert_eq!(calamine_to_json(&Data::Empty), Value::Null);
    }

    #[test]
    fn test_format_markdown_table() {
        let rows = vec![
            vec![
                Value::String("Name".to_string()),
                Value::String("Age".to_string()),
            ],
            vec![Value::String("Alice".to_string()), Value::Number(30.into())],
            vec![Value::String("Bob".to_string()), Value::Number(25.into())],
        ];
        let md = format_markdown_table(&rows, 20);
        assert!(md.contains("Name"));
        assert!(md.contains("Alice"));
        assert!(md.contains("Bob"));
        assert!(md.contains("|"));
        assert!(md.contains("---"));
    }

    #[test]
    fn test_format_markdown_table_empty() {
        let md = format_markdown_table(&[], 20);
        assert_eq!(md, "(empty sheet)");
    }

    #[test]
    fn test_build_format_bold() {
        let spec = CellFormatSpec {
            range: "A1:A1".to_string(),
            bold: Some(true),
            italic: None,
            font_color: None,
            bg_color: None,
            number_format: None,
            border: None,
        };
        assert!(build_format(&spec).is_ok());
    }

    #[test]
    fn test_build_format_invalid_color() {
        let spec = CellFormatSpec {
            range: "A1:A1".to_string(),
            bold: None,
            italic: None,
            font_color: Some("not-a-color".to_string()),
            bg_color: None,
            number_format: None,
            border: None,
        };
        assert!(build_format(&spec).is_err());
    }

    // ── Integration tests ──

    /// Helper: create a temp dir with a simple .xlsx file using rust_xlsxwriter directly.
    fn create_test_xlsx(dir: &std::path::Path, filename: &str) -> std::path::PathBuf {
        let path = dir.join(filename);
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet().set_name("Data").unwrap();
        // Header row
        sheet.write_string(0, 0, "Name").unwrap();
        sheet.write_string(0, 1, "Age").unwrap();
        sheet.write_string(0, 2, "City").unwrap();
        // Data rows
        sheet.write_string(1, 0, "Alice").unwrap();
        sheet.write_number(1, 1, 30.0).unwrap();
        sheet.write_string(1, 2, "NYC").unwrap();
        sheet.write_string(2, 0, "Bob").unwrap();
        sheet.write_number(2, 1, 25.0).unwrap();
        sheet.write_string(2, 2, "LA").unwrap();
        workbook.save(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn test_read_excel_tool_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let xlsx_path = create_test_xlsx(tmp.path(), "test.xlsx");

        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ReadExcelTool::new(service);

        let output = tool
            .call(ReadExcelArgs {
                path: xlsx_path.to_str().unwrap().to_string(),
                sheet: None,
                range: None,
                max_rows: None,
            })
            .await
            .unwrap();

        assert_eq!(output.sheet_names, vec!["Data"]);
        assert_eq!(output.selected_sheet, "Data");
        assert_eq!(output.total_rows, 3); // header + 2 data rows
        assert_eq!(output.rows_returned, 3);
        assert_eq!(output.data.len(), 3);
        // Check header row
        assert_eq!(output.data[0][0], Value::String("Name".to_string()));
        assert_eq!(output.data[0][1], Value::String("Age".to_string()));
        // Check data row — calamine reads numbers as floats
        assert_eq!(output.data[1][0], Value::String("Alice".to_string()));
        assert!(output.markdown_preview.contains("Name"));
    }

    #[tokio::test]
    async fn test_read_excel_tool_specific_sheet() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("multi.xlsx");

        // Create xlsx with two sheets
        let mut workbook = Workbook::new();
        let s1 = workbook.add_worksheet().set_name("Sheet1").unwrap();
        s1.write_string(0, 0, "A").unwrap();
        let s2 = workbook.add_worksheet().set_name("Sheet2").unwrap();
        s2.write_string(0, 0, "B").unwrap();
        s2.write_string(1, 0, "C").unwrap();
        workbook.save(&path).unwrap();

        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ReadExcelTool::new(service);

        let output = tool
            .call(ReadExcelArgs {
                path: path.to_str().unwrap().to_string(),
                sheet: Some("Sheet2".to_string()),
                range: None,
                max_rows: None,
            })
            .await
            .unwrap();

        assert_eq!(output.selected_sheet, "Sheet2");
        assert_eq!(output.data.len(), 2);
        assert_eq!(output.data[0][0], Value::String("B".to_string()));
    }

    #[tokio::test]
    async fn test_read_excel_tool_with_range() {
        let tmp = tempfile::tempdir().unwrap();
        let xlsx_path = create_test_xlsx(tmp.path(), "range_test.xlsx");

        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ReadExcelTool::new(service);

        let output = tool
            .call(ReadExcelArgs {
                path: xlsx_path.to_str().unwrap().to_string(),
                sheet: None,
                range: Some("A1:B2".to_string()),
                max_rows: None,
            })
            .await
            .unwrap();

        // Should only return 2 rows, 2 columns
        assert_eq!(output.data.len(), 2);
        assert_eq!(output.data[0].len(), 2);
        assert_eq!(output.data[0][0], Value::String("Name".to_string()));
        assert_eq!(output.data[0][1], Value::String("Age".to_string()));
    }

    #[tokio::test]
    async fn test_read_excel_tool_max_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let xlsx_path = create_test_xlsx(tmp.path(), "max_rows.xlsx");

        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ReadExcelTool::new(service);

        // max_rows=1 means keep header + 1 data row = 2 rows total (from 3)
        let output = tool
            .call(ReadExcelArgs {
                path: xlsx_path.to_str().unwrap().to_string(),
                sheet: None,
                range: None,
                max_rows: Some(1),
            })
            .await
            .unwrap();

        assert_eq!(output.rows_returned, 2); // header + 1 data row
        assert_eq!(output.total_rows, 3); // original 3 rows
        assert_eq!(output.data.len(), 2);
    }

    #[tokio::test]
    async fn test_read_excel_nonexistent_file() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ReadExcelTool::new(service);

        let result = tool
            .call(ReadExcelArgs {
                path: tmp.path().join("nope.xlsx").to_str().unwrap().to_string(),
                sheet: None,
                range: None,
                max_rows: None,
            })
            .await;

        assert!(result.is_err());
    }

    #[test]
    fn test_write_sheet_roundtrip() {
        // Test the write_sheet helper directly — no approval needed
        let mut workbook = Workbook::new();
        let spec = SheetSpec {
            name: "TestSheet".to_string(),
            data: vec![
                vec![
                    Value::String("X".to_string()),
                    Value::String("Y".to_string()),
                ],
                vec![Value::Number(1.into()), Value::Number(2.into())],
                vec![Value::Number(3.into()), Value::Number(4.into())],
            ],
            column_widths: vec![15.0, 15.0],
            formatting: Some(SheetFormatting {
                header_bold: true,
                auto_filter: true,
                freeze_top_row: true,
            }),
            cell_formats: vec![],
            merged_cells: vec![],
            formulas: vec![],
        };
        let rows = write_sheet(&mut workbook, &spec).unwrap();
        assert_eq!(rows, 3);

        // Save and re-read with calamine
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("roundtrip.xlsx");
        workbook.save(&path).unwrap();

        let mut wb = open_workbook_auto(&path).unwrap();
        let sheets = wb.sheet_names().to_vec();
        assert_eq!(sheets, vec!["TestSheet"]);
        let range = wb.worksheet_range("TestSheet").unwrap();
        let rows_vec: Vec<Vec<Data>> = range.rows().map(|r| r.to_vec()).collect();
        assert_eq!(rows_vec.len(), 3);
        assert_eq!(rows_vec[0][0], Data::String("X".to_string()));
        // Numbers are written and read back
        assert!(matches!(rows_vec[1][0], Data::Float(f) if (f - 1.0).abs() < f64::EPSILON));
    }

    #[test]
    fn test_write_sheet_with_formulas() {
        let mut workbook = Workbook::new();
        let spec = SheetSpec {
            name: "Formulas".to_string(),
            data: vec![
                vec![
                    Value::String("A".to_string()),
                    Value::String("B".to_string()),
                    Value::String("Sum".to_string()),
                ],
                vec![
                    Value::Number(10.into()),
                    Value::Number(20.into()),
                    Value::Null,
                ],
            ],
            column_widths: vec![],
            formatting: None,
            cell_formats: vec![],
            merged_cells: vec![],
            formulas: vec![FormulaSpec {
                cell: "C2".to_string(),
                formula: "=A2+B2".to_string(),
            }],
        };
        let rows = write_sheet(&mut workbook, &spec).unwrap();
        assert_eq!(rows, 2);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("formulas.xlsx");
        workbook.save(&path).unwrap();

        // Verify file is valid xlsx (calamine can open it)
        let wb = open_workbook_auto(&path);
        assert!(wb.is_ok());
    }

    #[test]
    fn test_write_sheet_with_cell_formats() {
        let mut workbook = Workbook::new();
        let spec = SheetSpec {
            name: "Formatted".to_string(),
            data: vec![
                vec![
                    Value::String("Header".to_string()),
                    Value::String("Value".to_string()),
                ],
                vec![
                    Value::String("Price".to_string()),
                    Value::Number(serde_json::Number::from_f64(99.99).unwrap().into()),
                ],
            ],
            column_widths: vec![],
            formatting: None,
            cell_formats: vec![CellFormatSpec {
                range: "B2:B2".to_string(),
                bold: Some(true),
                italic: None,
                font_color: Some("FF0000".to_string()),
                bg_color: None,
                number_format: Some("#,##0.00".to_string()),
                border: Some("thin".to_string()),
            }],
            merged_cells: vec![],
            formulas: vec![],
        };
        assert!(write_sheet(&mut workbook, &spec).is_ok());

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("formatted.xlsx");
        workbook.save(&path).unwrap();
        assert!(open_workbook_auto(&path).is_ok());
    }

    #[tokio::test]
    async fn test_edit_excel_read_modify_write_roundtrip() {
        // Test the edit flow: create xlsx, read with calamine, modify, write back
        let tmp = tempfile::tempdir().unwrap();
        let xlsx_path = create_test_xlsx(tmp.path(), "edit_test.xlsx");

        // Read the original with calamine
        let mut wb = open_workbook_auto(&xlsx_path).unwrap();
        let sheet_names = wb.sheet_names().to_vec();
        assert_eq!(sheet_names, vec!["Data"]);

        let range = wb.worksheet_range("Data").unwrap();
        let mut rows: Vec<Vec<Value>> = range
            .rows()
            .map(|r| r.iter().map(calamine_to_json).collect())
            .collect();

        // Modify: change Alice's age to 31
        rows[1][1] = Value::Number(31.into());

        // Write back with rust_xlsxwriter
        let output_path = tmp.path().join("edited.xlsx");
        let mut workbook = Workbook::new();
        let spec = SheetSpec {
            name: "Data".to_string(),
            data: rows.clone(),
            column_widths: vec![],
            formatting: None,
            cell_formats: vec![],
            merged_cells: vec![],
            formulas: vec![],
        };
        write_sheet(&mut workbook, &spec).unwrap();
        workbook.save(&output_path).unwrap();

        // Read back and verify the edit
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = ReadExcelTool::new(service);
        let output = tool
            .call(ReadExcelArgs {
                path: output_path.to_str().unwrap().to_string(),
                sheet: None,
                range: None,
                max_rows: None,
            })
            .await
            .unwrap();

        assert_eq!(output.data.len(), 3);
        assert_eq!(output.data[1][0], Value::String("Alice".to_string()));
        // The edited value — calamine reads it back as float
        let age = &output.data[1][1];
        match age {
            Value::Number(n) => assert_eq!(n.as_f64().unwrap(), 31.0),
            _ => panic!("Expected number, got {:?}", age),
        }
    }
}
