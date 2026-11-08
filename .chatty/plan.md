# AGE-538 plan

## Acceptance criteria
- With no workspace, nothing is downloaded and the home directory is never written.
- With a workspace, files land only in `<workspace>/daytona_outputs/`; an existing file is never replaced (a numbered name is used).
- The 4 new tests pass and existing `tools::daytona_tool` tests still pass.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only helpers.rs, mod.rs, tests.rs under crates/chatty-core/src/tools/daytona_tool/ change (besides these .chatty notes).

## Steps
1. [x] helpers.rs: DOWNLOAD_DIR_NAME, download_dir, numbered_name
2. [ ] mod.rs: save_new_file + download_generated_files rewrite
3. [ ] tests.rs: four tests
