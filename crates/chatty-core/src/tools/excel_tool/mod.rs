mod edit;
mod parsing;
mod read;
mod write;

pub use edit::{EditExcelTool, EditOperation};
pub use read::ReadExcelTool;
pub use write::{CellFormatSpec, FormulaSpec, SheetFormatting, SheetSpec, WriteExcelTool};

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

#[cfg(test)]
mod tests {
    use super::parsing::*;
    use super::write::{write_sheet, CellFormatSpec, FormulaSpec, SheetFormatting, SheetSpec};
    use super::*;
    use calamine::{Data, Reader, open_workbook_auto};
    use rust_xlsxwriter::Workbook;
    use serde_json::Value;
    use std::sync::Arc;

    use crate::services::filesystem_service::FileSystemService;
    use rig::tool::Tool;

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
            .call(read::ReadExcelArgs {
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
            .call(read::ReadExcelArgs {
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
            .call(read::ReadExcelArgs {
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
            .call(read::ReadExcelArgs {
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
            .call(read::ReadExcelArgs {
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
            .call(read::ReadExcelArgs {
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
