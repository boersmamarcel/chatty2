# AGE-1087 plan

Acceptance criteria:
- `resolve_output_path("~")` returns Err, no panic.
- `~/` paths that normalize outside home return Err; inside home resolve as before.
- 3 new tests + existing path_utils tests pass.
- clippy -D warnings and fmt pass.
- Only crates/chatty-core/src/tools/path_utils.rs changes (besides .chatty/ state).

Steps:
1. [ ] Fix `~` handling in path_utils.rs, update doc comment, add 3 tests (commit: pending)
