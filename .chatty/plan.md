# AGE-1090 plan

## Acceptance criteria
- parse_cell_ref never panics; `$A$1` is read as `A1`; non-letters and columns past XFD are InvalidCellRef.
- New test and updated test_col_from_letters pass; other tools::excel_tool tests still pass.
- clippy -D warnings and cargo fmt --all --check pass.
- Only crates/chatty-core/src/tools/excel_tool/parsing.rs and mod.rs change (plus .chatty state files).

## Steps
1. [ ] Implement the change in parsing.rs and mod.rs incl. tests
2. [ ] Remove state files (done later by lead)
