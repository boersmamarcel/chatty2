use rig_agent::tool::{Tool, ToolContext, ToolExecutionError};
use rust_xlsxwriter::{Format, Formula, Workbook};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::models::write_approval_store::{PendingWriteApprovals, WriteOperation};
use crate::services::filesystem_service::FileSystemService;
use crate::settings::models::execution_settings::ApprovalMode;

use super::ExcelToolError;
use super::formula_eval::{CellInput, Evaluation, WorkbookModel, XlValue, evaluate};
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
    /// Formula cells whose value could not be precomputed (AGE-1790).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Clone)]
pub struct WriteExcelTool {
    service: Arc<FileSystemService>,
    approval_mode: ApprovalMode,
    pending_approvals: PendingWriteApprovals,
    /// Where a relative `path` goes, inside the workspace (AGE-1747).
    /// `None`: relative to the workspace root, as always.
    output_root: Option<std::path::PathBuf>,
}

impl WriteExcelTool {
    pub fn new(
        service: Arc<FileSystemService>,
        approval_mode: ApprovalMode,
        pending_approvals: PendingWriteApprovals,
    ) -> Self {
        Self {
            service,
            approval_mode,
            pending_approvals,
            output_root: None,
        }
    }

    /// Place a relative `path` under `output_root` (the execution
    /// settings' `output_root`, AGE-1747); `None` keeps today's behaviour.
    pub fn with_output_root(mut self, output_root: Option<std::path::PathBuf>) -> Self {
        self.output_root = output_root;
        self
    }
}

/// The cells `write_sheet` stores for `specs`, sheet by sheet, in the same
/// order it writes them: data, then merged ranges (rust_xlsxwriter puts ""
/// in the first cell and blanks the rest), then formulas (AGE-1790).
/// Cell references `write_sheet` would refuse are skipped here.
pub(super) fn workbook_model(specs: &[SheetSpec]) -> WorkbookModel {
    let mut model = WorkbookModel::default();
    for spec in specs {
        let s = model.add_sheet(&spec.name);
        for (r, row) in spec.data.iter().enumerate() {
            for (c, val) in row.iter().enumerate() {
                if let (Ok(r), Ok(c)) = (u32::try_from(r), u16::try_from(c)) {
                    model.set(s, r, c, CellInput::Value(XlValue::from_json(val)));
                }
            }
        }
        for merge_range in &spec.merged_cells {
            let Ok(((sr, sc), (er, ec))) = parse_range(merge_range) else {
                continue;
            };
            for r in sr..=er {
                for c in sc..=ec {
                    model.set(s, r, c, CellInput::Value(XlValue::Empty));
                }
            }
            model.set(s, sr, sc, CellInput::Value(XlValue::Text(String::new())));
        }
        for f in &spec.formulas {
            if let Ok((r, c)) = parse_cell_ref(&f.cell) {
                model.set(s, r, c, CellInput::Formula(f.formula.clone()));
            }
        }
    }
    model
}

/// Precompute every formula of `specs` for its cached result (AGE-1790).
pub(super) fn evaluate_specs(specs: &[SheetSpec]) -> Evaluation {
    let mut evaluation = evaluate(&workbook_model(specs));
    evaluation.demote_unstorable_text();
    evaluation
}

/// The `warning` for formulas that could not be precomputed.
pub(super) fn formula_warning(evaluation: &Evaluation) -> Option<String> {
    evaluation.summary().map(|s| {
        format!(
            "{s}. They are stored with a cached value of 0: Excel recalculates them on open, \
             but previews and readers that trust cached values show 0."
        )
    })
}

/// Write one `SheetSpec` to a workbook, storing each formula's precomputed
/// result from `evaluation` (sheet `sheet_index`). Returns the number of
/// data rows written.
pub(super) fn write_sheet(
    workbook: &mut Workbook,
    spec: &SheetSpec,
    evaluation: &Evaluation,
    sheet_index: usize,
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
        let mut formula = Formula::new(f.formula.as_str());
        if let Some(v) = evaluation.result(sheet_index, r, c) {
            formula = formula.set_result(v.to_cached_result());
        }
        sheet
            .write_formula(r, c, formula)
            .map_err(|e| ExcelToolError::WriteError(e.to_string()))?;
    }

    Ok(total_rows)
}

impl Tool for WriteExcelTool {
    const NAME: &'static str = "write_excel";
    type Error = ExcelToolError;
    type Args = WriteExcelArgs;
    type Output = WriteExcelOutput;

    fn description(&self) -> String {
        "Create a new Excel (.xlsx) file with structured data and formatting.\n\
                         Supports multiple sheets, bold/italic/color formatting, column widths,\n\
                         auto-filters, frozen panes, merged cells, number formats, borders, and formulas.\n\
                         \n\
                         Formula results are precomputed; any formula cell the tool cannot compute is listed in the result, and previews may show 0 for it until Excel recalculates.\n\
                         \n\
                         Requires user approval before writing.\n\
                         \n\
                         Examples:\n\
                         - Simple data: {\"path\": \"output.xlsx\", \"sheets\": [{\"name\": \"Sheet1\", \"data\": [[\"Name\",\"Age\"],[\"Alice\",30]]}]}\n\
                         - With formatting: {\"path\": \"report.xlsx\", \"sheets\": [{\"name\": \"Sales\", \"data\": [...], \"formatting\": {\"header_bold\": true, \"auto_filter\": true, \"freeze_top_row\": true}, \"column_widths\": [20, 15, 12]}]}"
                .to_string()
            + &crate::tools::path_utils::output_root_note(self.output_root.as_deref(), "path")
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
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
                                        "anyOf": [
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
        })
    }

    /// Keep the real failure text in front of the user and the model:
    /// rig's default `map_error` redacts it to "the tool failed" (AGE-187).
    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        crate::tools::map_tool_error(Self::NAME, error)
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let path =
            crate::tools::path_utils::under_output_root(self.output_root.as_deref(), &args.path);
        let canonical = self.service.resolve_new_path(&path).await?;
        // `symlink_metadata`: a link is not shown as its target (AGE-1113).
        let is_overwrite = crate::services::no_follow_write::entry_exists(&canonical);

        // Request approval
        let approved = request_write_approval(
            &self.pending_approvals,
            &self.approval_mode,
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

        let evaluation = evaluate_specs(&args.sheets);
        for (i, spec) in args.sheets.iter().enumerate() {
            total_rows += write_sheet(&mut workbook, spec, &evaluation, i)?;
        }
        let warning = formula_warning(&evaluation);

        // In memory, then never through a symlink at the target (AGE-1113).
        let bytes = workbook
            .save_to_buffer()
            .map_err(|e| ExcelToolError::WriteError(format!("Failed to save: {}", e)))?;
        crate::services::no_follow_write::write_no_follow(&canonical, &bytes)
            .map_err(|e| ExcelToolError::WriteError(format!("Failed to save: {}", e)))?;

        Ok(WriteExcelOutput {
            path: canonical.display().to_string(),
            sheets_written: sheets_count,
            total_rows,
            message: format!(
                "Created Excel file '{}' with {} sheet(s) and {} total rows.",
                args.path, sheets_count, total_rows
            ),
            warning,
        })
    }
}
