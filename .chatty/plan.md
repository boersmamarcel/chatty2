# AGE-424: Remove the dead ContextPressureEvent::Relieved

## Acceptance criteria
- ContextPressureEvent in crates/chatty-core/src/token_budget/snapshot.rs has exactly two variants, HighPressure and CriticalPressure.
- `git grep -n Relieved -- crates docs docs-site` is empty.
- `cargo test -p chatty-core --lib token_budget`, `cargo clippy -p chatty-core --all-targets -- -D warnings`, `cargo fmt --all --check` pass.
- Only snapshot.rs changes (plus .chatty state files).

## Steps
1. [ ] Delete the Relieved variant + doc comment, reword the ~L197 comment, grep, run checks.
