use calamine::{Reader, open_workbook_auto};
use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::services::filesystem_service::FileSystemService;

use super::ExcelToolError;
use super::parsing::{calamine_to_json, format_markdown_table, parse_range};

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
