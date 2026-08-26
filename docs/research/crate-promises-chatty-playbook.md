# chatty-playbook — what this crate promises (AGE-26)

**Status:** stub. Stage A is AGE-17 (after AGE-5).

## Promises

- Concrete `PlaybookError`.
- **Bounded growth** — eviction / section caps are first-class (not an unbounded experiment knob).
- Deterministic merge (`apply`) once human-written; no LLM in the merge path.
- Stable bullet ordering for prompt-cache friendliness.
- `#![forbid(unsafe_code)]`, MSRV declared.
- Does not pull Harbor / Stage B sandboxes into the app binary.

## Non-promises

- Persistence backend is chatty-core `memory_service` / memvid — this crate owns structure + merge, not a new store.
