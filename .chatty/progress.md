# Progress (append-only)

- 2026-11-07: plan written; earlier coder sessions disconnected before committing anything.
- 2026-11-07: step 1 done in this commit: streaming call_streaming tracks saw_terminal; a stream ending with no completed/failed status now fails with "stream ended before the task finished". No tests yet (steps 2-3); cargo not run.
- 2026-11-07: steps 2-3 done: tests stream_ending_without_terminal_status_fails and stream_with_completed_status_stays_success (local scripted SSE server on 127.0.0.1); rustfmt --check clean; cargo not run (tester runs it).
