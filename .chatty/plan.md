# AGE-1132: a failed best_of ends the leader's run

## Acceptance criteria
- Every solver failing sets the budget_stop channel with `best_of: every attempt failed` (test `all_failed_stops_the_run`).
- Final message names best_of, says no answer was produced, no `Budget` wording (test `best_of_stop_text_names_best_of`).
- Budget stop text unchanged (test `budget_stop_text_unchanged`); existing budget-stop tests pass.
- The AllFailed error is still returned.
- Checks: cargo test -p chatty-core --lib best_of; ... budget_stop; clippy -D warnings; fmt --check.

## Steps
1. [x] budget_stop.rs: best_of stop text + tests `best_of_stop_text_names_best_of`, `budget_stop_text_unchanged` (commit: "budget_stop: word a best_of failure as one, not a spent budget")
2. [ ] invoke_agent_tool.rs seam + best_of AllFailed branch calls it + test `all_failed_stops_the_run`
3. [ ] checks pass
