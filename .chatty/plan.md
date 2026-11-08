# AGE-535 slice: file-extension guard

## Acceptance criteria
- write_docx refuses any path whose extension is not docx (any case) before resolve_new_path; write_pptx same for pptx.
- A refused call leaves the target file unchanged.
- .docx/.pptx paths incl. upper-case still work.
- 2 new tests pass; existing tools::docx_tool, tools::pptx_tool, services::pptx_render tests still pass.
- cargo clippy -p chatty-core --all-targets -- -D warnings and cargo fmt --all --check pass.
- Only docx_tool/write.rs, pptx_tool/write.rs, docx_tool/mod.rs, pptx_tool/mod.rs change (plus .chatty/). Constructors unchanged.

## Steps
1. [ ] Add extension guard, description sentence and tests to both tools; run checks; commit (commit: pending)
