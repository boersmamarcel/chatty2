# Plan AGE-1097

## Acceptance criteria
- Binary download is created with create_new: never replaces a file, never writes through a symlink.
- `unique_path` removed; nothing calls it.
- 3 new tests pass; other tools::fetch_tool tests pass.
- clippy -D warnings and cargo fmt --all --check pass.
- Only fetch_tool.rs changes (plus .chatty state files, removed later).

## Steps
1. [x] Replace unique_path with save_new_file, use it in handle_binary_response, add tests (commit: see git log)
