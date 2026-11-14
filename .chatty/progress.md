# Progress
## 2026-11-14
- Steps 1-3 were already committed in 17d85e1f; resumed from step 4.
- Decision: the stop applies to every leader holding the InvokeAgentTool. It cannot be limited cleanly, and best_of is the only caller.
- Seam: all_failed_stops_the_run uses an InvokeAgentTool with no known agents, so solvers fail at once with no model or broker.

## 2026-11-14 (fix round 1)
- Decision (criterion 4) now lives in the doc comment on BEST_OF_FAILED_HEADLINE.
- on_outcome: the best_of branch only sets the headline when the rendered result contains `best_of: every attempt failed`.
- Tests: an_all_failed_best_of_result_stops_the_run, another_best_of_error_does_not_stop_the_run.
- invoke_agent_tool.rs: stop_on_budget doc mentions the best_of_failed: stop.
