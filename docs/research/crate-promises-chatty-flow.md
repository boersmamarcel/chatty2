# chatty-flow — what this crate promises (AGE-26)

**Status:** stub. Stage A is AGE-13 (after AGE-5 / GEPA scaffolding).

## Promises

- Concrete `FlowError`.
- Interpreter cancellable when wired to LLM / tool calls.
- `WorkflowRepr` trait keeps representation swappable (`IrRepr` first; Monty later).
- `#![forbid(unsafe_code)]`, MSRV declared.
- Zero dependency on `chatty-eval`.

## Non-promises

- MCTS search lives in `chatty-optimize`, not here.
- Does not replace `sub_agent_tool` / `invoke_agent_tool` — composes over them.
