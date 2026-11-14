# AGE-1132 r2: best_of all-failed stops the leader

## Acceptance criteria
- BestOfError::AllFailed sets the stop channel with `best_of_failed: every attempt failed` before returning, error unchanged.
- budget_stop::final_message answers with BEST_OF_STOP for `best_of_failed:` headlines; others unchanged.
- Tests `all_failed_stops_the_run`, `best_of_stop_text_names_best_of`.

## Steps
1. [x] budget_stop.rs: headline const, BEST_OF_STOP, final_message branch, test (17d85e1f)
2. [x] invoke_agent_tool.rs: reuse stop_on_budget (pub(crate)) + test accessor (17d85e1f)
3. [x] best_of_tool.rs: call stop before AllFailed, test (17d85e1f)
4. [x] run checks, record plan/progress
