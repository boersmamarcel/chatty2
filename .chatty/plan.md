# AGE-1092 plan

## Acceptance criteria
- A test covers pdf_to_image with no output_dir and checks the PNG lands in pdf_thumbnail::get_thumbnail_dir().
- Both tests reaching the thumbnail dir hold thumbnail_dir_test_lock(), no guard held across an .await.
- tools::pdf_to_image_tool, services::pdf_thumbnail, services::pptx_render tests pass 10x in a row (default threads).
- cargo clippy -p chatty-core --all-targets -- -D warnings and cargo fmt --all --check pass.
- Only pdf_to_image_tool.rs changes (plus .chatty state files, removed later).

## Steps
1. [ ] Add test_no_output_dir_renders_into_the_session_thumbnail_dir
2. [ ] Convert test_all_pages_out_of_range_errors to locked #[test] + runtime.block_on
3. [ ] fmt, tests (10x), clippy; commit
