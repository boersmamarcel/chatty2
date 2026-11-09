# AGE-1096: write_docx/write_pptx ask for write approval (ADR-0023 §2)

## Acceptance criteria
- WriteDocxTool/WritePptxTool take `with_write_approval(mode, pending)`; `new` unchanged (None).
- `call` asks via `request_write_approval` after `resolve_new_path`, before any directory/file is created; denial returns `Write denied by user`.
- Agent factory always sets approval on both tools (only built when pending approvals exist).
- Tests `write_docx_asks_before_writing_under_always_ask` and `write_pptx_asks_before_writing_under_always_ask` pass.
- Only 5 source files change (plus .chatty state files); chatty-gpui untouched.

## Steps
1. [x] docx_tool/write.rs approval (3f17d712)
2. [x] pptx_tool/write.rs approval (b963a60b)
3. [x] agent_factory/mod.rs wiring (b963a60b)
4. [x] docx/pptx always-ask tests (15a72c5f)
5. [x] Run checks: tools::docx_tool, tools::pptx_tool, services::pptx_render, factories::agent_factory, clippy, fmt
