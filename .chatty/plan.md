# AGE-549: DockerSandbox::create leaks container on later failure

## Acceptance criteria
- In `create`, any error after create_container succeeded leads to a forced remove_container before the error is returned.
- The original error (with context) is returned; a failed removal is only logged.
- The 2 new tests pass.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only crates/chatty-core/src/sandbox/docker.rs changes (plus .chatty state files).

## Steps
1. [x] Add `cleanup_on_err`, wrap start+inspect in an async block with cleanup, add 2 tests (commit: see git log, AGE-549 fix)
