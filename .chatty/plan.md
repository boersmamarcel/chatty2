# AGE-1059: repeated pr_checks waits are not a loop

## Acceptance criteria
- `on_tool_completed` returns None for repeated `invoke_agent` calls whose prompt is a JSON `{"tool": "pr_checks", ...}`; `loop_pivot_count` does not grow.
- Other repeated delegations (other github tools, plain-text prompts) still trigger "LOOP DETECTED".
- Signature of `on_tool_completed` unchanged; only agent_loop_guard.rs (+ .chatty) changes.

## Steps
1. [ ] Add `is_ci_wait`, exempt it in `on_tool_completed`, add two tests, run checks (commit: see progress.md)
