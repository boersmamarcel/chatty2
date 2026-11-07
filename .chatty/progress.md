# Progress (append-only)

- 2026-11-07: plan written; earlier coder sessions disconnected before committing anything.
- 2026-11-07: step 1 done in this commit: streaming call_streaming tracks saw_terminal; a stream ending with no completed/failed status now fails with "stream ended before the task finished". No tests yet (steps 2-3); cargo not run.
