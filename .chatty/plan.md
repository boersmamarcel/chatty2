# AGE-1051 plan
Acceptance: (a) test: remote agent stream sends an artifact then ends with no status -> success false, error `stream ended before the task finished`; (b) test: stream with `completed` status stays success; (c) existing invoke_agent tests pass. Checks: cargo test -p chatty-core --lib invoke_agent; cargo clippy -p chatty-core --all-targets -- -D warnings; cargo fmt --all --check.
Steps:
1. [ ] In crates/chatty-core/src/tools/invoke_agent_tool.rs streaming call (~line 828) track a seen terminal status (completed/failed); after loop, if none and success still true: success=false, error_msg=Some("stream ended before the task finished".into())
2. [ ] test: no terminal status fails
3. [ ] test: completed stays success
