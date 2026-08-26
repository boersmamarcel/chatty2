# chatty-playbook — what this crate promises (AGE-26)

**Status:** stub. Stage A is AGE-17 (after AGE-5).
See also [`semver-policy.md`](semver-policy.md).

## Promises

- Concrete `PlaybookError`.
- **Bounded growth** — eviction / section caps are first-class (not an unbounded experiment knob).
- Deterministic merge (`apply`) once human-written; no LLM in the merge path.
- Stable bullet ordering for prompt-cache friendliness.
- `#![forbid(unsafe_code)]`, MSRV declared — CI-enforced.
- Does not pull Harbor / Stage B sandboxes into the app binary.
- Does not depend on `chatty-optimize` — CI-enforced.

## Non-promises

- Persistence backend is chatty-core `memory_service` / memvid — this crate owns structure + merge, not a new store.
