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

/// RUSTSEC-2026-0436 / AGE-949: malformed OLE2 (CFB) containers used to
/// panic or allocate without bound in calamine's own CFB parser. Each header below
/// is a few hundred bytes; calamine 0.38 must return an error for all of
/// them, never panic or abort.
#[test]
fn malformed_ole2_xls_is_an_error_not_a_panic() {
    const MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    let header = |patch: &dyn Fn(&mut Vec<u8>)| {
        let mut h = vec![0u8; 512];
        h[..8].copy_from_slice(&MAGIC);
        h[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes()); // minor version
        h[0x1A..0x1C].copy_from_slice(&0x0003u16.to_le_bytes()); // major version 3
        h[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes()); // little endian
        h[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes()); // 512-byte sectors
        h[0x20..0x22].copy_from_slice(&6u16.to_le_bytes()); // 64-byte mini sectors
        patch(&mut h);
        h
    };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("magic only", MAGIC.to_vec()),
        ("truncated header", header(&|h| h.truncate(100))),
        (
            "huge FAT sector count",
            header(&|h| h[0x2C..0x30].copy_from_slice(&u32::MAX.to_le_bytes())),
        ),
        (
            "huge DIFAT chain",
            header(&|h| {
                h[0x44..0x48].copy_from_slice(&0u32.to_le_bytes());
                h[0x48..0x4C].copy_from_slice(&u32::MAX.to_le_bytes());
            }),
        ),
        (
            "huge sector shift",
            header(&|h| h[0x1E..0x20].copy_from_slice(&0xFFFFu16.to_le_bytes())),
        ),
        (
            "huge mini FAT count",
            header(&|h| {
                h[0x40..0x44].copy_from_slice(&u32::MAX.to_le_bytes());
                h[0x3C..0x40].copy_from_slice(&0u32.to_le_bytes());
            }),
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in cases {
        let path = dir.path().join("bad.xls");
        std::fs::write(&path, &bytes).unwrap();
        let outcome = std::panic::catch_unwind(|| calamine::open_workbook_auto(&path).map(|_| ()));
        match outcome {
            Ok(Err(_)) => {}
            Ok(Ok(())) => panic!("case '{name}': malformed OLE2 opened as a workbook"),
            Err(_) => panic!("case '{name}': calamine panicked on malformed OLE2"),
        }
    }
}

use super::edit::EditExcelArgs;
use super::parsing::*;
use super::write::{
    CellFormatSpec, FormulaSpec, SheetFormatting, SheetSpec, WriteExcelArgs, evaluate_specs,
    write_sheet,
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

fn read_style_signature(path: &std::path::Path, sheet: &str, cell: &str) -> (bool, String, String) {
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
    let rows = write_sheet(
        &mut workbook,
        &spec,
        &evaluate_specs(std::slice::from_ref(&spec)),
        0,
    )
    .unwrap();
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
    let rows = write_sheet(
        &mut workbook,
        &spec,
        &evaluate_specs(std::slice::from_ref(&spec)),
        0,
    )
    .unwrap();
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
    assert!(
        write_sheet(
            &mut workbook,
            &spec,
            &evaluate_specs(std::slice::from_ref(&spec)),
            0
        )
        .is_ok()
    );

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
    write_sheet(
        &mut workbook,
        &spec,
        &evaluate_specs(std::slice::from_ref(&spec)),
        0,
    )
    .unwrap();
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

// ── Precomputed formula results (AGE-1790) ──

fn sheet(name: &str, data: Vec<Vec<Value>>, formulas: &[(&str, &str)]) -> SheetSpec {
    SheetSpec {
        name: name.to_string(),
        data,
        column_widths: vec![],
        formatting: None,
        cell_formats: vec![],
        merged_cells: vec![],
        formulas: formulas
            .iter()
            .map(|(cell, formula)| FormulaSpec {
                cell: cell.to_string(),
                formula: formula.to_string(),
            })
            .collect(),
    }
}

/// Put `rows` into a grid starting at row `top` (0-based), column A.
fn grid(top: usize, rows: Vec<Vec<Value>>) -> Vec<Vec<Value>> {
    let mut out = vec![Vec::new(); top];
    out.extend(rows);
    out
}

async fn write_tool(dir: &std::path::Path) -> WriteExcelTool {
    let service = Arc::new(FileSystemService::new(dir.to_str().unwrap()).await.unwrap());
    WriteExcelTool::new(
        service,
        ApprovalMode::AutoApproveAll,
        WriteApprovalStore::new().get_pending_approvals(),
    )
}

async fn edit_tool(dir: &std::path::Path) -> EditExcelTool {
    let service = Arc::new(FileSystemService::new(dir.to_str().unwrap()).await.unwrap());
    EditExcelTool::new(
        service,
        ApprovalMode::AutoApproveAll,
        WriteApprovalStore::new().get_pending_approvals(),
    )
}

/// The cached value calamine reads at `cell` of `sheet`.
fn cached(path: &std::path::Path, sheet: &str, cell: &str) -> Data {
    let (r, c) = parse_cell_ref(cell).unwrap();
    let mut wb = open_workbook_auto(path).unwrap();
    let range = wb.worksheet_range(sheet).unwrap();
    range
        .get_value((r, u32::from(c)))
        .cloned()
        .unwrap_or(Data::Empty)
}

fn assert_number(path: &std::path::Path, sheet: &str, cell: &str, want: f64) {
    match cached(path, sheet, cell) {
        Data::Float(f) => assert!((f - want).abs() < 1e-6, "{sheet}!{cell}: {f} != {want}"),
        Data::Int(i) => assert_eq!(i as f64, want, "{sheet}!{cell}"),
        other => panic!("{sheet}!{cell}: expected {want}, got {other:?}"),
    }
}

/// The GDPval-style P&L: unit prices by VLOOKUP into an assumptions
/// sheet, revenue sums, cross-sheet rates, and a summary back on the
/// assumptions sheet that reads the P&L. Written by the stock
/// rust_xlsxwriter path, every one of these read back as 0.
fn pnl_sheets() -> Vec<SheetSpec> {
    let n = |x: f64| serde_json::json!(x);
    let s = |x: &str| Value::String(x.to_string());
    let assumptions = sheet(
        "Assumptions",
        grid(
            4,
            vec![
                vec![s("Product"), s("Unit price")],
                vec![s("Widget"), n(25.0)],
                vec![s("Gadget"), n(40.0)],
                vec![s("Gizmo"), n(15.5)],
                vec![s("Doohickey"), n(65.0)],
                vec![],
                vec![s("COGS rate"), n(0.375)],
                vec![s("Opex"), n(40_893.5)],
                vec![s("Tax rate"), n(0.15)],
                vec![],
                vec![s("Net income (from P&L)")],
            ],
        ),
        &[("B15", "='P&L'!F17")],
    );
    let pnl = sheet(
        "P&L",
        grid(
            6,
            vec![
                vec![
                    Value::Null,
                    Value::Null,
                    s("Product"),
                    s("Units"),
                    s("Price"),
                    s("Revenue"),
                ],
                vec![Value::Null, Value::Null, s("Widget"), n(20_000.0)],
                vec![Value::Null, Value::Null, s("Gadget"), n(15_000.0)],
                vec![Value::Null, Value::Null, s("Gizmo"), n(20_000.0)],
                vec![Value::Null, Value::Null, s("Doohickey"), n(4_000.0)],
                vec![Value::Null, Value::Null, s("Revenue")],
                vec![Value::Null, Value::Null, s("COGS")],
                vec![Value::Null, Value::Null, s("Gross profit")],
                vec![Value::Null, Value::Null, s("Opex")],
                vec![Value::Null, Value::Null, s("Pre-tax income")],
                vec![Value::Null, Value::Null, s("Net income")],
            ],
        ),
        &[
            ("E8", "=VLOOKUP(C8,Assumptions!$A$6:$B$9,2,FALSE)"),
            ("E9", "=VLOOKUP(C9,Assumptions!$A$6:$B$9,2,FALSE)"),
            ("E10", "=VLOOKUP(C10,Assumptions!$A$6:$B$9,2,FALSE)"),
            ("E11", "=VLOOKUP(C11,Assumptions!$A$6:$B$9,2,FALSE)"),
            ("F8", "=D8*E8"),
            ("F9", "=D9*E9"),
            ("F10", "=D10*E10"),
            ("F11", "=D11*E11"),
            ("F12", "=SUM(F8:F11)"),
            ("F13", "=F12*Assumptions!B11"),
            ("F14", "=F12-F13"),
            ("F15", "=Assumptions!B12"),
            ("F16", "=F14-F15"),
            ("F17", "=ROUND(F16*(1-Assumptions!B13),0)"),
        ],
    );
    vec![assumptions, pnl]
}

#[tokio::test]
async fn write_excel_stores_computed_formula_results() {
    let tmp = tempfile::tempdir().unwrap();
    let output = write_tool(tmp.path())
        .await
        .call(
            &mut ToolContext::new(),
            WriteExcelArgs {
                path: "pnl.xlsx".to_string(),
                sheets: pnl_sheets(),
            },
        )
        .await
        .unwrap();
    assert_eq!(output.warning, None, "every formula should be precomputed");

    let path = tmp.path().join("pnl.xlsx");
    assert_number(&path, "P&L", "E8", 25.0);
    assert_number(&path, "P&L", "E11", 65.0);
    assert_number(&path, "P&L", "F12", 1_670_000.0);
    assert_number(&path, "P&L", "F14", 1_043_750.0);
    assert_number(&path, "P&L", "F17", 852_428.0);
    assert_number(&path, "Assumptions", "B15", 852_428.0);
    // Literal cells are unchanged.
    assert_eq!(
        cached(&path, "P&L", "C8"),
        Data::String("Widget".to_string())
    );
    if let Ok(dir) = std::env::var("CHATTY_XLSX_TEST_OUT") {
        std::fs::copy(&path, std::path::Path::new(&dir).join("pnl.xlsx")).unwrap();
    }
}

#[tokio::test]
async fn write_excel_lists_formulas_it_cannot_compute() {
    let tmp = tempfile::tempdir().unwrap();
    let output = write_tool(tmp.path())
        .await
        .call(
            &mut ToolContext::new(),
            WriteExcelArgs {
                path: "x.xlsx".to_string(),
                sheets: vec![sheet(
                    "Sheet1",
                    vec![vec![serde_json::json!(1), Value::Null, Value::Null]],
                    &[("B1", "=XLOOKUP(1,A1:A1,A1:A1)"), ("C1", "=A1*2")],
                )],
            },
        )
        .await
        .unwrap();
    let warning = output.warning.expect("the XLOOKUP cell is listed");
    assert!(
        warning.starts_with("1 formula cell(s) could not be precomputed: Sheet1!B1."),
        "{warning}"
    );
    assert!(
        warning.contains("Excel recalculates them on open"),
        "{warning}"
    );
    let path = tmp.path().join("x.xlsx");
    assert_number(&path, "Sheet1", "B1", 0.0);
    assert_number(&path, "Sheet1", "C1", 2.0);
}

#[tokio::test]
async fn write_excel_lists_a_cycle_and_does_not_hang() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = write_tool(tmp.path()).await;
    let mut context = ToolContext::new();
    let call = tool.call(
        &mut context,
        WriteExcelArgs {
            path: "cycle.xlsx".to_string(),
            sheets: vec![sheet("Sheet1", vec![], &[("A1", "=B1+1"), ("B1", "=A1+1")])],
        },
    );
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), call)
        .await
        .expect("a cycle must not hang")
        .unwrap();
    let warning = output.warning.expect("the cycle is listed");
    assert!(
        warning.starts_with("2 formula cell(s) could not be precomputed: Sheet1!A1, Sheet1!B1."),
        "{warning}"
    );
    let path = tmp.path().join("cycle.xlsx");
    assert_number(&path, "Sheet1", "A1", 0.0);
    assert_number(&path, "Sheet1", "B1", 0.0);
}

/// edit_excel recomputes every formula over the edited workbook, in both
/// modes: the style-preserving one (umya) and the rewrite fallback that
/// `format_range` forces.
#[tokio::test]
async fn edit_excel_updates_dependent_cached_values() {
    let tmp = tempfile::tempdir().unwrap();
    write_tool(tmp.path())
        .await
        .call(
            &mut ToolContext::new(),
            WriteExcelArgs {
                path: "model.xlsx".to_string(),
                sheets: pnl_sheets(),
            },
        )
        .await
        .unwrap();
    let input = tmp.path().join("model.xlsx");
    let tool = edit_tool(tmp.path()).await;

    // Style-preserving: raise the Widget price; the untouched VLOOKUP,
    // sums and the cross-sheet summary follow.
    let output = tool
        .call(
            &mut ToolContext::new(),
            EditExcelArgs {
                path: input.to_str().unwrap().to_string(),
                output_path: Some("edited.xlsx".to_string()),
                operations: vec![
                    EditOperation::SetCell {
                        sheet: "Assumptions".to_string(),
                        cell: "B6".to_string(),
                        value: serde_json::json!(30),
                    },
                    EditOperation::SetFormula {
                        sheet: "P&L".to_string(),
                        cell: "G12".to_string(),
                        formula: "COUNTIF(E8:E11,\">=30\")".to_string(),
                    },
                    EditOperation::SetFormula {
                        sheet: "P&L".to_string(),
                        cell: "G13".to_string(),
                        formula: "NOPE(F12)".to_string(),
                    },
                ],
            },
        )
        .await
        .unwrap();
    let edited = tmp.path().join("edited.xlsx");
    assert_number(&edited, "P&L", "E8", 30.0);
    assert_number(&edited, "P&L", "F12", 1_770_000.0);
    assert_number(&edited, "P&L", "F14", 1_106_250.0);
    assert_number(&edited, "Assumptions", "B15", 905_553.0);
    assert_number(&edited, "P&L", "G12", 3.0);
    assert_number(&edited, "P&L", "G13", 0.0);
    let warning = output.warning.expect("NOPE is listed");
    assert!(
        warning.starts_with("1 formula cell(s) could not be precomputed: 'P&L'!G13."),
        "{warning}"
    );
    assert!(
        warning.contains("possibly stale if this edit changed their inputs"),
        "{warning}"
    );

    // Rewrite fallback (format_range): calamine drops the file's
    // formulas, so only the ones this edit sets are formulas; they are
    // computed against the edited cells. (A sheet that starts at A1:
    // the fallback reads calamine's range as if it did.)
    let plain = create_test_xlsx(tmp.path(), "plain.xlsx");
    let fallback = tool
        .call(
            &mut ToolContext::new(),
            EditExcelArgs {
                path: plain.to_str().unwrap().to_string(),
                output_path: Some("rewritten.xlsx".to_string()),
                operations: vec![
                    EditOperation::SetCell {
                        sheet: "Data".to_string(),
                        cell: "B2".to_string(),
                        value: serde_json::json!(40),
                    },
                    EditOperation::SetFormula {
                        sheet: "Data".to_string(),
                        cell: "D2".to_string(),
                        formula: "=B2*2".to_string(),
                    },
                    EditOperation::SetFormula {
                        sheet: "Data".to_string(),
                        cell: "D3".to_string(),
                        formula: "=SUM(B2:B3)+D2".to_string(),
                    },
                    serde_json::from_value(serde_json::json!({
                        "type": "format_range", "sheet": "Data", "range": "A1:C1", "bold": true
                    }))
                    .unwrap(),
                ],
            },
        )
        .await
        .unwrap();
    let rewritten = tmp.path().join("rewritten.xlsx");
    assert_number(&rewritten, "Data", "D2", 80.0);
    assert_number(&rewritten, "Data", "D3", 145.0);
    assert!(
        !fallback
            .warning
            .unwrap()
            .contains("could not be precomputed"),
        "D2 and D3 are computed"
    );
}

/// AGE-1791: a sheet whose first used cell is B3 keeps every cell where it
/// was through the rewrite fallback.
#[tokio::test]
async fn fallback_keeps_cell_positions_when_data_starts_at_b3() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("offset.xlsx");
    let mut book = rust_xlsxwriter::Workbook::new();
    let sheet = book.add_worksheet().set_name("S").unwrap();
    sheet.write_string(2, 1, "head").unwrap();
    sheet.write_number(2, 2, 1.0).unwrap();
    sheet.write_number(3, 1, 7.0).unwrap();
    sheet.write_number(4, 3, 9.0).unwrap();
    book.save(&input).unwrap();
    edit_tool(tmp.path())
        .await
        .call(
            &mut ToolContext::new(),
            EditExcelArgs {
                path: input.to_str().unwrap().to_string(),
                output_path: Some("out.xlsx".to_string()),
                operations: vec![
                    serde_json::from_value(serde_json::json!({
                        "type": "format_range", "sheet": "S", "range": "B3:B3", "bold": true
                    }))
                    .unwrap(),
                ],
            },
        )
        .await
        .unwrap();
    let out = tmp.path().join("out.xlsx");
    assert_eq!(cached(&out, "S", "B3"), Data::String("head".into()));
    assert_number(&out, "S", "C3", 1.0);
    assert_number(&out, "S", "B4", 7.0);
    assert_number(&out, "S", "D5", 9.0);
    assert_eq!(cached(&out, "S", "A1"), Data::Empty);
}

/// AGE-1791: a formula inside a formatted range stays a formula, with its
/// computed cached value.
#[tokio::test]
async fn fallback_format_range_keeps_formulas() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = create_test_xlsx(tmp.path(), "plain.xlsx");
    edit_tool(tmp.path())
        .await
        .call(
            &mut ToolContext::new(),
            EditExcelArgs {
                path: plain.to_str().unwrap().to_string(),
                output_path: Some("out.xlsx".to_string()),
                operations: vec![
                    EditOperation::SetCell {
                        sheet: "Data".to_string(),
                        cell: "B2".to_string(),
                        value: serde_json::json!(21),
                    },
                    EditOperation::SetFormula {
                        sheet: "Data".to_string(),
                        cell: "C2".to_string(),
                        formula: "B2*2".to_string(),
                    },
                    serde_json::from_value(serde_json::json!({
                        "type": "format_range", "sheet": "Data", "range": "B2:D2", "bold": true
                    }))
                    .unwrap(),
                ],
            },
        )
        .await
        .unwrap();
    let out = tmp.path().join("out.xlsx");
    let book = umya_spreadsheet::reader::xlsx::read(&out).unwrap();
    let cell = book.sheet_by_name("Data").unwrap().cell("C2").unwrap();
    assert!(cell.is_formula(), "C2 must stay a formula");
    assert_eq!(cell.formula(), "B2*2");
    assert_number(&out, "Data", "C2", 42.0);
}

/// AGE-1791: one leading `=` is not stored in the umya path, and the
/// evaluator still computes the formula.
#[tokio::test]
async fn set_formula_strips_one_leading_equals_in_umya_path() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = create_test_xlsx(tmp.path(), "plain.xlsx");
    edit_tool(tmp.path())
        .await
        .call(
            &mut ToolContext::new(),
            EditExcelArgs {
                path: plain.to_str().unwrap().to_string(),
                output_path: Some("out.xlsx".to_string()),
                operations: vec![
                    EditOperation::SetCell {
                        sheet: "Data".to_string(),
                        cell: "A1".to_string(),
                        value: serde_json::json!(5),
                    },
                    EditOperation::SetFormula {
                        sheet: "Data".to_string(),
                        cell: "E1".to_string(),
                        formula: "=A1*2".to_string(),
                    },
                ],
            },
        )
        .await
        .unwrap();
    let out = tmp.path().join("out.xlsx");
    let book = umya_spreadsheet::reader::xlsx::read(&out).unwrap();
    let cell = book.sheet_by_name("Data").unwrap().cell("E1").unwrap();
    assert_eq!(cell.formula(), "A1*2");
    assert_number(&out, "Data", "E1", 10.0);
}

/// AGE-1791: in the fallback, a later set_cell/set_range replaces an
/// earlier set_formula on the same cell.
#[tokio::test]
async fn fallback_set_cell_replaces_an_earlier_formula() {
    let tmp = tempfile::tempdir().unwrap();
    let plain = create_test_xlsx(tmp.path(), "plain.xlsx");
    edit_tool(tmp.path())
        .await
        .call(
            &mut ToolContext::new(),
            EditExcelArgs {
                path: plain.to_str().unwrap().to_string(),
                output_path: Some("out.xlsx".to_string()),
                operations: vec![
                    EditOperation::SetFormula {
                        sheet: "Data".to_string(),
                        cell: "D2".to_string(),
                        formula: "B2*2".to_string(),
                    },
                    EditOperation::SetFormula {
                        sheet: "Data".to_string(),
                        cell: "D3".to_string(),
                        formula: "B3*2".to_string(),
                    },
                    EditOperation::SetCell {
                        sheet: "Data".to_string(),
                        cell: "D2".to_string(),
                        value: serde_json::json!(5),
                    },
                    EditOperation::SetRange {
                        sheet: "Data".to_string(),
                        start_cell: "D3".to_string(),
                        data: vec![vec![serde_json::json!(6)]],
                    },
                    serde_json::from_value(serde_json::json!({
                        "type": "format_range", "sheet": "Data", "range": "A1:A1", "bold": true
                    }))
                    .unwrap(),
                ],
            },
        )
        .await
        .unwrap();
    let out = tmp.path().join("out.xlsx");
    let book = umya_spreadsheet::reader::xlsx::read(&out).unwrap();
    let ws = book.sheet_by_name("Data").unwrap();
    assert!(!ws.cell("D2").unwrap().is_formula());
    assert!(!ws.cell("D3").unwrap().is_formula());
    assert_number(&out, "Data", "D2", 5.0);
    assert_number(&out, "Data", "D3", 6.0);
}

/// AGE-1791: in the fallback, delete_rows moves formulas and formats up
/// with the rows below, and drops those inside the deleted rows.
#[tokio::test]
async fn fallback_delete_rows_shifts_formulas_and_formats() {
    let tmp = tempfile::tempdir().unwrap();
    let input = tmp.path().join("rows.xlsx");
    let mut book = rust_xlsxwriter::Workbook::new();
    let sheet = book.add_worksheet().set_name("S").unwrap();
    for r in 0..5u32 {
        sheet.write_number(r, 0, f64::from(r + 1)).unwrap();
    }
    book.save(&input).unwrap();
    let fmt = |range: &str| {
        serde_json::from_value(serde_json::json!({
            "type": "format_range", "sheet": "S", "range": range, "bold": true
        }))
        .unwrap()
    };
    edit_tool(tmp.path())
        .await
        .call(
            &mut ToolContext::new(),
            EditExcelArgs {
                path: input.to_str().unwrap().to_string(),
                output_path: Some("out.xlsx".to_string()),
                operations: vec![
                    EditOperation::SetFormula {
                        sheet: "S".to_string(),
                        cell: "B2".to_string(),
                        formula: "1+1".to_string(),
                    },
                    EditOperation::SetFormula {
                        sheet: "S".to_string(),
                        cell: "B5".to_string(),
                        formula: "7*10".to_string(),
                    },
                    fmt("A2:A2"),
                    fmt("A5:A5"),
                    EditOperation::DeleteRows {
                        sheet: "S".to_string(),
                        start_row: 2,
                        count: 1,
                    },
                ],
            },
        )
        .await
        .unwrap();
    let out = tmp.path().join("out.xlsx");
    // Old row 5 is now row 4: its value 5, its formula, its bold.
    assert_number(&out, "S", "A4", 5.0);
    assert_number(&out, "S", "B4", 70.0);
    assert_eq!(cached(&out, "S", "B2"), Data::Empty);
    assert_eq!(cached(&out, "S", "B5"), Data::Empty);
    let book = umya_spreadsheet::reader::xlsx::read(&out).unwrap();
    let ws = book.sheet_by_name("S").unwrap();
    assert!(ws.cell("B4").unwrap().is_formula());
    let bold = |c: &str| {
        ws.cell(c)
            .unwrap()
            .style()
            .font()
            .is_some_and(|f| f.font_bold().val())
    };
    assert!(bold("A4"), "format moved with its row");
    assert!(!bold("A2"), "the deleted row's format is gone");
}
