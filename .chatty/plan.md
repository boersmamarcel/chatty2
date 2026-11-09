# AGE-1088 plan

## Acceptance criteria
- clean_title never panics on non-ASCII; a title over 100 characters keeps first 97 chars plus `...`.
- The 4 new tests pass.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only title_generator.rs changes (plus .chatty/ state files, removed later).

## Steps
1. [x] Replace byte-slicing arm in clean_title with char-counting; add doc line. (commit: AGE-1088 fix commit)
2. [x] Add `#[cfg(test)] mod tests` with the 4 tests at end of title_generator.rs. (commit: AGE-1088 fix commit)
