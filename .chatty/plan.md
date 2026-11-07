# Plan: A2A client failures surface as failures (AGE-1050)

## Criteria
- A non-SSE JSON reply containing a JSON-RPC error, or a task in a failed/canceled state, yields exactly one StatusUpdate (failed, is_final, with message) and no artifact.
- An SSE block with an error and no result yields Err containing its message.
- 3 new tests: (a) JSON-RPC error non-SSE reply, (b) failed/canceled task state non-SSE reply, (c) SSE error block without result.
- Existing a2a_client tests, `cargo clippy -D warnings` and `cargo fmt --check` pass.

## Steps
- [x] 1. Plan
- [ ] 2. Code change in crates/chatty-core/src/services/a2a_client.rs
- [ ] 3. Tests (a, b, c)
- [ ] 4. Run checks (a2a_client tests, clippy -D warnings, fmt)
