use calamine::{Reader, open_workbook_auto};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use rust_xlsxwriter::Workbook;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tracing::warn;

use crate::models::write_approval_store::{PendingWriteApprovals, WriteOperation};
use crate::services::filesystem_service::FileSystemService;

use super::ExcelToolError;
use super::parsing::{
    build_format, calamine_to_json, parse_cell_ref, parse_range, write_cell_value,
};
use super::write::CellFormatSpec;

// Re-use the write approval helper from filesystem_write_tool
use super::super::filesystem_write_tool::request_write_approval;

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
                         Original formatting, macros, charts, images, and formulas may be lost.\n\
                         Only use when modifications are needed.\n\
                         \n\
                         Supported operations: set_cell, set_range, add_sheet, delete_rows, set_formula, format_range\n\
                         \n\
                         Examples:\n\
                         - Set a cell: {\"path\": \"data.xlsx\", \"operations\": [{\"type\": \"set_cell\", \"sheet\": \"Sheet1\", \"cell\": \"B2\", \"value\": 42}]}\n\
                         - Add a sheet: {\"path\": \"data.xlsx\", \"operations\": [{\"type\": \"add_sheet\", \"name\": \"NewSheet\", \"data\": [[\"A\",\"B\"],[1,2]]}]}\n\
                         - Delete rows: {\"path\": \"data.xlsx\", \"operations\": [{\"type\": \"delete_rows\", \"sheet\": \"Sheet1\", \"start_row\": 5, \"count\": 3}]}"
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
                        "description": "Array of edit operations to apply in order",
                        "items": {
                            "oneOf": [
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "const": "set_cell" },
                                        "sheet": { "type": "string" },
                                        "cell": { "type": "string", "description": "Cell reference, e.g. 'B2'" },
                                        "value": { "description": "New cell value" }
                                    },
                                    "required": ["type", "sheet", "cell", "value"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "const": "set_range" },
                                        "sheet": { "type": "string" },
                                        "start_cell": { "type": "string", "description": "Top-left cell, e.g. 'A1'" },
                                        "data": { "type": "array", "items": { "type": "array" } }
                                    },
                                    "required": ["type", "sheet", "start_cell", "data"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "const": "add_sheet" },
                                        "name": { "type": "string" },
                                        "data": { "type": "array", "items": { "type": "array" } }
                                    },
                                    "required": ["type", "name"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "const": "delete_rows" },
                                        "sheet": { "type": "string" },
                                        "start_row": { "type": "integer", "description": "1-based row index" },
                                        "count": { "type": "integer" }
                                    },
                                    "required": ["type", "sheet", "start_row", "count"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "const": "set_formula" },
                                        "sheet": { "type": "string" },
                                        "cell": { "type": "string" },
                                        "formula": { "type": "string" }
                                    },
                                    "required": ["type", "sheet", "cell", "formula"]
                                },
                                {
                                    "type": "object",
                                    "properties": {
                                        "type": { "const": "format_range" },
                                        "sheet": { "type": "string" },
                                        "range": { "type": "string" },
                                        "bold": { "type": "boolean" },
                                        "italic": { "type": "boolean" },
                                        "font_color": { "type": "string" },
                                        "bg_color": { "type": "string" },
                                        "number_format": { "type": "string" },
                                        "border": { "type": "string" }
                                    },
                                    "required": ["type", "sheet", "range"]
                                }
                            ]
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
