# Progress
- Step 1 done: `BEST_OF_FAILED` headline const; `final_message` words a best_of headline as "best_of failed on every attempt; no answer was produced." (no budget wording, no "Here is what was finished" list); budget text untouched, pinned by test.
- Decision (leader scope), FINAL: apply to every leader. best_of is the only caller of this branch and it cannot be limited to crosscheck cleanly.
- Fix round 1: the stop hook now ends the run on a failed best_of (hook branch + `map_error` on BestOfTool, since rig redacted the text); refusals/hang-ups for a best_of headline drop the budget wording (`stop_text`/`stop_note`). New test `a_failed_best_of_ends_the_run_at_the_next_call`.
