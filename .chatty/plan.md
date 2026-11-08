# AGE-548: fetch binary download + error-body read

## Acceptance criteria
- A binary response whose Content-Length exceeds 10 MiB is refused before any body is read.
- A binary response with no length is read at most to 10 MiB, then refused; nothing saved.
- A non-success response's body is read at most to 256 KiB.
- The 4 new tests pass; existing tools::fetch_tool tests still pass.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only crates/chatty-core/src/tools/fetch_tool.rs changes (besides .chatty state files).

## Steps
1. [ ] Implement the change (const, read_capped, handle_binary_response, error-body path).
2. [ ] Add the 4 tests.
