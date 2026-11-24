mod edit;
mod formula_eval;
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
mod tests;
