use rust_xlsxwriter::{Format, FormatBorder};
use serde_json::Value;

use super::ExcelToolError;

/// Convert column letter(s) to 0-based index (A=0, B=1, ..., Z=25, AA=26, ...).
pub(super) fn col_from_letters(s: &str) -> u16 {
    let mut col: u16 = 0;
    for b in s.bytes() {
        col = col * 26 + (b - b'A') as u16 + 1;
    }
    col - 1
}

/// Parse a cell reference like "A1" or "AB123" into (row, col) 0-based indices.
pub(super) fn parse_cell_ref(s: &str) -> Result<(u32, u16), ExcelToolError> {
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
pub(super) type CellCoord = (u32, u16);

/// Parse a range like "A1:D10" into ((start_row, start_col), (end_row, end_col)).
pub(super) fn parse_range(s: &str) -> Result<(CellCoord, CellCoord), ExcelToolError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err(ExcelToolError::InvalidRange(s.to_string()));
    }
    let start = parse_cell_ref(parts[0])?;
    let end = parse_cell_ref(parts[1])?;
    Ok((start, end))
}

/// Convert a calamine `Data` cell to a JSON value.
pub(super) fn calamine_to_json(cell: &calamine::Data) -> Value {
    match cell {
        calamine::Data::Int(n) => Value::Number((*n).into()),
        calamine::Data::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        calamine::Data::String(s) => Value::String(s.clone()),
        calamine::Data::Bool(b) => Value::Bool(*b),
        calamine::Data::DateTime(dt) => Value::String(format!("{}", dt.as_f64())),
        calamine::Data::DateTimeIso(s) => Value::String(s.clone()),
        calamine::Data::DurationIso(s) => Value::String(s.clone()),
        calamine::Data::Error(e) => Value::String(format!("#ERR:{:?}", e)),
        calamine::Data::Empty => Value::Null,
    }
}

/// Format rows as a markdown table string. Shows at most `max_rows` data rows.
pub(super) fn format_markdown_table(rows: &[Vec<Value>], max_rows: usize) -> String {
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

pub(super) fn cell_display(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Parse a hex color string like "FF0000" or "#FF0000" into a u32 RGB value.
pub(super) fn parse_hex_color(s: &str) -> Result<u32, ExcelToolError> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    u32::from_str_radix(hex, 16).map_err(|_| ExcelToolError::InvalidColor(s.to_string()))
}

/// Write a JSON value to a worksheet cell.
pub(super) fn write_cell_value(
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

/// Build a `rust_xlsxwriter::Format` from a `CellFormatSpec`.
pub(super) fn build_format(spec: &super::CellFormatSpec) -> Result<Format, ExcelToolError> {
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
