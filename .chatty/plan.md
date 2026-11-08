# AGE-528: send the unknown-tool nudge once on Desktop/InteractiveTui

## Acceptance criteria
- Both new tests pass; all existing `session::` tests still pass (incl. unknown_tool_call_queues_a_corrective_follow_up_instead_of_stopping).
- On Desktop and InteractiveTui, a turn sent with the unknown-tool nudge text queues no further FollowUp on another UnknownToolCall.
- Nudge text the model sees is byte-for-byte unchanged.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only session/handler.rs, session/mod.rs, session/tests.rs change (plus .chatty/ state files).

## Steps
1. [x] handler.rs/mod.rs: add UNKNOWN_TOOL_CALL_FOLLOW_UP_PREFIX, use it in the format!, update already_asked_to_retry doc, export it and add to prepare_turn array. Commit: step 1 commit (see git log).
2. [x] tests.rs: add the_unknown_tool_call_nudge_is_sent_once_on_desktop_and_tui and unknown_tool_call_nudge_is_a_protocol_follow_up. Commit: step 2 commit (see git log).
3. [ ] Run session:: tests, clippy, fmt check; fix issues.
