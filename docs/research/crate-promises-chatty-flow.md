# chatty-flow — what this crate promises (AGE-26)

**Status:** stub. Stage A is AGE-13 (after AGE-5 / GEPA scaffolding).
See also [`semver-policy.md`](semver-policy.md).

## Promises

- Concrete `FlowError`.
- Interpreter cancellable when wired to LLM / tool calls.
- `WorkflowRepr` trait keeps representation swappable (`IrRepr` first; Monty later).
- `#![forbid(unsafe_code)]`, MSRV declared — CI-enforced.
- Does not pull Harbor / Stage B sandboxes into the app binary.
- Does not depend on `chatty-optimize` — CI-enforced.

## Non-promises

- MCTS search lives in `chatty-optimize`, not here.
- Does not replace `sub_agent_tool` / `invoke_agent_tool` — composes over them.
