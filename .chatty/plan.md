# AGE-989: a stream-error turn must still save its calls' usage

## Acceptance
- Both new tests pass and all existing `session::` tests (goldens included) pass.
- Any StreamChunk::Error or raw stream Err emits TokenUsage for completed calls before Error when it made calls.
- If aggregate TurnUsage already arrived, no second TokenUsage on later error.
- clippy -D warnings and fmt pass.
- Only crates/chatty-core/src/session/handler.rs and tests.rs change (plus these state files).

## Steps
1. [x] handler fix in handler.rs (emit_calls_usage on every error) - commit: "Save calls' usage on every stream-error turn (AGE-989)"
2. [ ] unit test in handler.rs
3. [ ] session test in tests.rs
4. [ ] run checks
