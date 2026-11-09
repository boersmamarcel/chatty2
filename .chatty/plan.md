# AGE-1096: write_docx / write_pptx raise human.approve

## Acceptance criteria
- write_docx and write_pptx call request_write_approval before writing: under AlwaysAsk an approval is raised, denial writes nothing.
- WriteDocxTool::new / WritePptxTool::new keep signatures; chatty-gpui unchanged.
- 2 new tests pass; existing tools::docx_tool, tools::pptx_tool, services::pptx_render, factories::agent_factory tests still pass.
- cargo clippy -p chatty-core --all-targets -- -D warnings and cargo fmt --all --check pass.
- Only touch: tools/docx_tool/{write,mod}.rs, tools/pptx_tool/{write,mod}.rs, factories/agent_factory/mod.rs (plus .chatty).

## Steps
1. [x] docx_tool/write.rs: with_write_approval + approval in call (commit: see git log)
2. [x] pptx_tool/write.rs: same
3. [x] agent_factory/mod.rs: build both writers with approval
4. [x] test write_docx_asks_before_writing_under_always_ask
5. [x] test write_pptx_asks_before_writing_under_always_ask
6. [ ] run tests, clippy, fmt
