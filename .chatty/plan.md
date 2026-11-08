# AGE-509 plan

## Acceptance criteria
- `thumbnail_dir_test_lock` exists only under #[cfg(test)]; pdf_thumbnail tests and the two named pptx_render tests take it.
- pdf_thumbnail + pptx_render tests pass 10 times in a row with default parallel threads.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only pdf_thumbnail.rs and pptx_render.rs change (plus .chatty state files, removed later).

## Steps
1. [ ] Implement shared test lock in pdf_thumbnail.rs and take it in pptx_render tests
2. [ ] Run checks (tests x10, clippy, fmt)
