# AGE-545: hosted SSE stream decodes only complete lines

## Acceptance criteria
- New test `a_multibyte_character_split_across_chunks_decodes_intact` passes; existing `session::hosted` tests still pass.
- A UTF-8 char split across two chunks decodes once, intact, no U+FFFD.
- `\r\n` line ends still accepted.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.

## Steps
1. [~] Buffer `SseFrames` as bytes, decode only complete lines, add test (crates/chatty-core/src/session/hosted.rs)
