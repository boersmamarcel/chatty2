use rig::completion::ToolDefinition;
use rig::tool::Tool;
use rust_xlsxwriter::{Format, Workbook};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::models::write_approval_store::{PendingWriteApprovals, WriteOperation};
use crate::services::filesystem_service::FileSystemService;

use super::ExcelToolError;
use super::parsing::{build_format, parse_cell_ref, parse_range, write_cell_value};

// Re-use the write approval helper from filesystem_write_tool
use super::super::filesystem_write_tool::request_write_approval;

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

/// Write one `SheetSpec` to a workbook. Returns the number of data rows written.
pub(super) fn write_sheet(
    workbook: &mut Workbook,
    spec: &SheetSpec,
) -> Result<usize, ExcelToolError> {
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
