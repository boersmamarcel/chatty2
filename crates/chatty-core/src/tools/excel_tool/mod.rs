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

/// The most cells a sheet's bounding box may span before it is read.
/// calamine turns a sheet into a dense grid from its first to its last cell,
/// so a 1 KB file with cells at A1 and XFD1048576 asks for 512 GiB and aborts
/// the process (AGE-1368). Five million cells is far beyond any sheet the
/// agent reads.
pub(super) const MAX_SHEET_CELLS: u64 = 5_000_000;

/// Refuse a sheet whose cells span more than [`MAX_SHEET_CELLS`], by
/// streaming its cell positions before `worksheet_range` builds the grid.
/// Only xlsx can be streamed; other formats are read as before.
pub(super) fn check_sheet_size<RS: std::io::Read + std::io::Seek>(
    workbook: &mut calamine::Sheets<RS>,
    name: &str,
) -> Result<(), String> {
    let calamine::Sheets::Xlsx(xlsx) = workbook else {
        return Ok(());
    };
    let mut cells = xlsx
        .worksheet_cells_reader(name)
        .map_err(|e| format!("failed to read sheet '{name}': {e}"))?;
    let (mut min_r, mut min_c, mut max_r, mut max_c) = (u32::MAX, u32::MAX, 0u32, 0u32);
    while let Some(cell) = cells
        .next_cell()
        .map_err(|e| format!("failed to read sheet '{name}': {e}"))?
    {
        let (r, c) = cell.get_position();
        min_r = min_r.min(r);
        min_c = min_c.min(c);
        max_r = max_r.max(r);
        max_c = max_c.max(c);
    }
    if min_r == u32::MAX {
        return Ok(());
    }
    let area = u64::from(max_r - min_r + 1) * u64::from(max_c - min_c + 1);
    if area > MAX_SHEET_CELLS {
        return Err(format!(
            "sheet '{name}' spans {area} cells (rows {}..{}, columns {}..{}); \
             refusing to load more than {MAX_SHEET_CELLS}",
            min_r + 1,
            max_r + 1,
            min_c + 1,
            max_c + 1
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// AGE-1368: two cells at opposite corners are refused before the grid
    /// is built; a normal sheet passes.
    #[test]
    fn a_sheet_spanning_too_many_cells_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.xlsx");
        let mut book = rust_xlsxwriter::Workbook::new();
        let sheet = book.add_worksheet().set_name("S").unwrap();
        sheet.write_string(0, 0, "a").unwrap();
        sheet.write_string(1_048_575, 16_383, "z").unwrap();
        book.save(&path).unwrap();
        let mut wb = calamine::open_workbook_auto(&path).unwrap();
        let refused = super::check_sheet_size(&mut wb, "S").unwrap_err();
        assert!(refused.contains("refusing"), "{refused}");

        let ok = dir.path().join("ok.xlsx");
        let mut book = rust_xlsxwriter::Workbook::new();
        let sheet = book.add_worksheet().set_name("S").unwrap();
        sheet.write_string(0, 0, "a").unwrap();
        sheet.write_string(99, 9, "z").unwrap();
        book.save(&ok).unwrap();
        let mut wb = calamine::open_workbook_auto(&ok).unwrap();
        assert!(super::check_sheet_size(&mut wb, "S").is_ok());
    }

    use super::edit::EditExcelArgs;
    use super::parsing::*;
    use super::write::{
        CellFormatSpec, FormulaSpec, SheetFormatting, SheetSpec, WriteExcelArgs, write_sheet,
    };
    use super::*;
    use calamine::{Data, Reader, open_workbook_auto};
    use rust_xlsxwriter::{Format, Workbook};
    use serde_json::Value;
    use std::sync::Arc;

    use crate::models::write_approval_store::WriteApprovalStore;
    use crate::services::filesystem_service::FileSystemService;
    use crate::settings::models::execution_settings::ApprovalMode;
    use rig_agent::tool::{Tool, ToolContext};

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
        assert_eq!(col_from_letters("A"), Some(0));
        assert_eq!(col_from_letters("B"), Some(1));
        assert_eq!(col_from_letters("Z"), Some(25));
        assert_eq!(col_from_letters("AA"), Some(26));
        assert_eq!(col_from_letters("AB"), Some(27));
        assert_eq!(col_from_letters("AZ"), Some(51));
        assert_eq!(col_from_letters("XFD"), Some(16_383));
        assert_eq!(col_from_letters("XFE"), None);
        assert_eq!(col_from_letters("A-"), None);
        assert_eq!(col_from_letters(""), None);
    }

    #[test]
    fn parse_cell_ref_handles_dollar_signs_and_refuses_odd_columns() {
        assert_eq!(parse_cell_ref("$A$1").unwrap(), (0, 0));
        assert_eq!(parse_cell_ref("$b3").unwrap(), (2, 1));
        assert_eq!(parse_cell_ref("XFD1").unwrap(), (0, 16_383));
        for bad in ["XFE1", "AAAAA1", "A-1", "Ä1", "#1"] {
            assert!(parse_cell_ref(bad).is_err(), "{bad} should be refused");
        }
        assert_eq!(parse_range("$A$1:$B$2").unwrap(), ((0, 0), (1, 1)));
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

    fn create_styled_test_xlsx(dir: &std::path::Path, filename: &str) -> std::path::PathBuf {
        let path = dir.join(filename);
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet().set_name("Data").unwrap();
        let style = Format::new()
            .set_bold()
            .set_background_color(0xFFF2CC)
            .set_font_color(0x9C0006);
        sheet
            .write_string_with_format(0, 0, "Name", &style)
            .unwrap();
        sheet.write_string(0, 1, "Age").unwrap();
        sheet.write_string(1, 0, "Alice").unwrap();
        sheet.write_number(1, 1, 30.0).unwrap();
        workbook.save(&path).unwrap();
        path
    }

    fn read_style_signature(
        path: &std::path::Path,
        sheet: &str,
        cell: &str,
    ) -> (bool, String, String) {
        let workbook = umya_spreadsheet::reader::xlsx::read(path).unwrap();
        let worksheet = workbook.sheet_by_name(sheet).unwrap();
        let styled_cell = worksheet.cell(cell).unwrap();
        let style = styled_cell.style();
        let bold = style.font().map(|font| font.bold()).unwrap_or(false);
        let font_color = style
            .font()
            .map(|font| font.color().argb_str())
            .unwrap_or_default();
        let bg_color = style
            .fill()
            .and_then(|fill| fill.pattern_fill())
            .and_then(|pattern| pattern.foreground_color())
            .map(|color| color.argb_str())
            .unwrap_or_default();
        (bold, font_color, bg_color)
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
            .call(
                &mut ToolContext::new(),
                read::ReadExcelArgs {
                    path: xlsx_path.to_str().unwrap().to_string(),
                    sheet: None,
                    range: None,
                    max_rows: None,
                },
            )
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
            .call(
                &mut ToolContext::new(),
                read::ReadExcelArgs {
                    path: path.to_str().unwrap().to_string(),
                    sheet: Some("Sheet2".to_string()),
                    range: None,
                    max_rows: None,
                },
            )
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
            .call(
                &mut ToolContext::new(),
                read::ReadExcelArgs {
                    path: xlsx_path.to_str().unwrap().to_string(),
                    sheet: None,
                    range: Some("A1:B2".to_string()),
                    max_rows: None,
                },
            )
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
            .call(
                &mut ToolContext::new(),
                read::ReadExcelArgs {
                    path: xlsx_path.to_str().unwrap().to_string(),
                    sheet: None,
                    range: None,
                    max_rows: Some(1),
                },
            )
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
            .call(
                &mut ToolContext::new(),
                read::ReadExcelArgs {
                    path: tmp.path().join("nope.xlsx").to_str().unwrap().to_string(),
                    sheet: None,
                    range: None,
                    max_rows: None,
                },
            )
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
                    Value::Number(serde_json::Number::from_f64(99.99).unwrap()),
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
            .call(
                &mut ToolContext::new(),
                read::ReadExcelArgs {
                    path: output_path.to_str().unwrap().to_string(),
                    sheet: None,
                    range: None,
                    max_rows: None,
                },
            )
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

    #[tokio::test]
    async fn test_write_excel_tool_creates_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let write_approvals = WriteApprovalStore::new().get_pending_approvals();
        let tool = WriteExcelTool::new(service, ApprovalMode::AutoApproveAll, write_approvals);

        let output = tool
            .call(
                &mut ToolContext::new(),
                WriteExcelArgs {
                    path: "example.xlsx".to_string(),
                    sheets: vec![SheetSpec {
                        name: "Sheet1".to_string(),
                        data: vec![vec![
                            Value::String("Name".to_string()),
                            Value::String("Age".to_string()),
                        ]],
                        column_widths: vec![],
                        formatting: None,
                        cell_formats: vec![],
                        merged_cells: vec![],
                        formulas: vec![],
                    }],
                },
            )
            .await
            .unwrap();

        let file_path = tmp.path().join("example.xlsx");
        assert!(file_path.exists());
        assert_eq!(output.sheets_written, 1);
    }

    #[tokio::test]
    async fn test_edit_excel_tool_creates_new_output_file() {
        let tmp = tempfile::tempdir().unwrap();
        let input_path = create_test_xlsx(tmp.path(), "input.xlsx");
        std::fs::create_dir_all(tmp.path().join("out")).unwrap();

        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let write_approvals = WriteApprovalStore::new().get_pending_approvals();
        let tool = EditExcelTool::new(
            service.clone(),
            ApprovalMode::AutoApproveAll,
            write_approvals,
        );

        tool.call(
            &mut ToolContext::new(),
            EditExcelArgs {
                path: input_path.to_str().unwrap().to_string(),
                output_path: Some("out/edited.xlsx".to_string()),
                operations: vec![EditOperation::SetCell {
                    sheet: "Data".to_string(),
                    cell: "B2".to_string(),
                    value: Value::Number(31.into()),
                }],
            },
        )
        .await
        .unwrap();

        let output_path = tmp.path().join("out/edited.xlsx");
        assert!(output_path.exists());

        let read_tool = ReadExcelTool::new(service);
        let output = read_tool
            .call(
                &mut ToolContext::new(),
                read::ReadExcelArgs {
                    path: output_path.to_str().unwrap().to_string(),
                    sheet: None,
                    range: None,
                    max_rows: None,
                },
            )
            .await
            .unwrap();
        match &output.data[1][1] {
            Value::Number(n) => assert_eq!(n.as_f64().unwrap(), 31.0),
            other => panic!("Expected numeric age in B2, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_edit_excel_tool_preserves_template_when_editing_in_place() {
        let tmp = tempfile::tempdir().unwrap();
        let input_path = create_styled_test_xlsx(tmp.path(), "template.xlsx");
        let style_before = read_style_signature(&input_path, "Data", "A1");

        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let write_approvals = WriteApprovalStore::new().get_pending_approvals();
        let tool = EditExcelTool::new(
            service.clone(),
            ApprovalMode::AutoApproveAll,
            write_approvals,
        );

        let output = tool
            .call(
                &mut ToolContext::new(),
                EditExcelArgs {
                    path: input_path.to_str().unwrap().to_string(),
                    output_path: None,
                    operations: vec![EditOperation::SetCell {
                        sheet: "Data".to_string(),
                        cell: "B2".to_string(),
                        value: Value::Number(31.into()),
                    }],
                },
            )
            .await
            .unwrap();

        assert_eq!(output.path, input_path.display().to_string());
        let style_after = read_style_signature(&input_path, "Data", "A1");
        assert_eq!(style_before, style_after);

        let read_tool = ReadExcelTool::new(service.clone());
        let edited = read_tool
            .call(
                &mut ToolContext::new(),
                read::ReadExcelArgs {
                    path: input_path.to_str().unwrap().to_string(),
                    sheet: None,
                    range: None,
                    max_rows: None,
                },
            )
            .await
            .unwrap();
        match &edited.data[1][1] {
            Value::Number(n) => assert_eq!(n.as_f64().unwrap(), 31.0),
            other => panic!("Expected numeric age in edited workbook, got {:?}", other),
        }
    }

    fn one_sheet(cell: &str) -> Vec<SheetSpec> {
        vec![SheetSpec {
            name: "Sheet1".to_string(),
            data: vec![vec![Value::String(cell.to_string())]],
            column_widths: vec![],
            formatting: None,
            cell_formats: vec![],
            merged_cells: vec![],
            formulas: vec![],
        }]
    }

    /// Symlinks planted in `workspace`: `link.xlsx` to a file outside it,
    /// `dangling.xlsx` to a not-yet-existing file outside it. Returns
    /// (the outside dir, the victim, the path the dangling link names).
    #[cfg(unix)]
    fn plant_links(
        workspace: &std::path::Path,
    ) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.xlsx");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, workspace.join("link.xlsx")).unwrap();
        let would_be = outside.path().join("created.xlsx");
        std::os::unix::fs::symlink(&would_be, workspace.join("dangling.xlsx")).unwrap();
        (outside, victim, would_be)
    }

    /// AGE-1113: write_excel refuses a symlink at the target (out of the
    /// workspace, or dangling) and writes nothing where it points.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_write_excel_refuses_a_symlink_at_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let (_outside, victim, would_be) = plant_links(tmp.path());
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = WriteExcelTool::new(
            service,
            ApprovalMode::AutoApproveAll,
            WriteApprovalStore::new().get_pending_approvals(),
        );
        for path in ["link.xlsx", "dangling.xlsx"] {
            let err = tool
                .call(
                    &mut ToolContext::new(),
                    WriteExcelArgs {
                        path: path.to_string(),
                        sheets: one_sheet("pwned"),
                    },
                )
                .await
                .err()
                .unwrap_or_else(|| panic!("{path}: written through the link"));
            assert!(
                err.to_string().contains("not a regular file"),
                "{path}: {err}"
            );
        }
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(!would_be.exists());
    }

    /// AGE-1113: write_excel still creates and overwrites a regular file,
    /// leaving no temp file behind.
    #[tokio::test]
    async fn test_write_excel_overwrites_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = WriteExcelTool::new(
            service,
            ApprovalMode::AutoApproveAll,
            WriteApprovalStore::new().get_pending_approvals(),
        );
        for cell in ["one", "two"] {
            tool.call(
                &mut ToolContext::new(),
                WriteExcelArgs {
                    path: "book.xlsx".to_string(),
                    sheets: one_sheet(cell),
                },
            )
            .await
            .unwrap();
        }
        let mut workbook = open_workbook_auto(tmp.path().join("book.xlsx")).unwrap();
        let range = workbook.worksheet_range("Sheet1").unwrap();
        assert_eq!(range.get((0, 0)), Some(&Data::String("two".to_string())));
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    /// AGE-1113: edit_excel refuses a symlink at `output_path` (out of the
    /// workspace, or dangling), in both the style-preserving and the
    /// legacy (format_range) mode, and writes nothing where it points.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_edit_excel_refuses_a_symlink_at_the_output_path() {
        let tmp = tempfile::tempdir().unwrap();
        let input_path = create_test_xlsx(tmp.path(), "input.xlsx");
        let (_outside, victim, would_be) = plant_links(tmp.path());
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = EditExcelTool::new(
            service,
            ApprovalMode::AutoApproveAll,
            WriteApprovalStore::new().get_pending_approvals(),
        );
        let set_cell = EditOperation::SetCell {
            sheet: "Data".to_string(),
            cell: "B2".to_string(),
            value: Value::Number(31.into()),
        };
        let format_range: EditOperation = serde_json::from_value(serde_json::json!({
            "type": "format_range", "sheet": "Data", "range": "A1:A1", "bold": true
        }))
        .unwrap();
        for output in ["link.xlsx", "dangling.xlsx"] {
            for op in [set_cell.clone(), format_range.clone()] {
                let err = tool
                    .call(
                        &mut ToolContext::new(),
                        EditExcelArgs {
                            path: input_path.to_str().unwrap().to_string(),
                            output_path: Some(output.to_string()),
                            operations: vec![op],
                        },
                    )
                    .await
                    .err()
                    .unwrap_or_else(|| panic!("{output}: written through the link"));
                assert!(
                    err.to_string().contains("not a regular file"),
                    "{output}: {err}"
                );
            }
        }
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
        assert!(!would_be.exists());
    }

    /// AGE-1113: edit_excel still overwrites an existing regular
    /// `output_path` in both modes, leaving no temp file behind.
    #[tokio::test]
    async fn test_edit_excel_overwrites_a_regular_output_file() {
        let tmp = tempfile::tempdir().unwrap();
        let input_path = create_test_xlsx(tmp.path(), "input.xlsx");
        let output_path = create_test_xlsx(tmp.path(), "out.xlsx");
        let service = Arc::new(
            FileSystemService::new(tmp.path().to_str().unwrap())
                .await
                .unwrap(),
        );
        let tool = EditExcelTool::new(
            service,
            ApprovalMode::AutoApproveAll,
            WriteApprovalStore::new().get_pending_approvals(),
        );
        let format_range: EditOperation = serde_json::from_value(serde_json::json!({
            "type": "format_range", "sheet": "Data", "range": "A1:A1", "bold": true
        }))
        .unwrap();
        for (age, extra) in [(31, None), (32, Some(format_range))] {
            let mut operations = vec![EditOperation::SetCell {
                sheet: "Data".to_string(),
                cell: "B2".to_string(),
                value: Value::Number(age.into()),
            }];
            operations.extend(extra);
            tool.call(
                &mut ToolContext::new(),
                EditExcelArgs {
                    path: input_path.to_str().unwrap().to_string(),
                    output_path: Some("out.xlsx".to_string()),
                    operations,
                },
            )
            .await
            .unwrap();
            let mut workbook = open_workbook_auto(&output_path).unwrap();
            let range = workbook.worksheet_range("Data").unwrap();
            assert_eq!(range.get((1, 1)), Some(&Data::Float(age as f64)));
        }
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2);
    }
}
