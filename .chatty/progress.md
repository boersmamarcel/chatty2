# Progress
## 2026-11-14
- Steps 1-3 were already committed in 17d85e1f; resumed from step 4.
- Decision: the stop applies to every leader holding the InvokeAgentTool. It cannot be limited cleanly, and best_of is the only caller.
- Seam: all_failed_stops_the_run uses an InvokeAgentTool with no known agents, so solvers fail at once with no model or broker.
