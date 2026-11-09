# AGE-1089 plan

Acceptance criteria:
- preview never panics on non-ASCII sheet names
- >150 chars keeps 150 chars plus `...`
- 2 new tests pass; existing tools::excel_tool tests pass
- clippy -D warnings and cargo fmt --all --check pass
- only edit.rs changes (besides state files)

Steps:
- [ ] Step 1: add approval_preview, use it in EditExcelTool::call, add 2 tests
