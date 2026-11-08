# AGE-519: memory previews cut at a char boundary

## Acceptance criteria
- 3 new tests pass; existing memory_query tests still pass.
- Neither function panics when the cut point is inside a multi-byte char.
- ASCII output unchanged (max 80 / 100 bytes).
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only remember_tool.rs and memory_query.rs change (plus .chatty state files).

## Steps
1. [ ] remember_tool.rs: `confirmation()` helper cut with floor_char_boundary(80) + 2 tests. Commit: (pending)
2. [ ] memory_query.rs: floor_char_boundary(100) + test `the_raw_fallback_is_cut_on_a_char_boundary`. Commit: (pending)
