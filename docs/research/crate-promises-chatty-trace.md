# chatty-trace — what this crate promises (AGE-26)

**Status:** stub. Contracts land after AGE-22 → AGE-5.

## Promises

- Concrete `TraceError` (no `Box<dyn Error>` in public APIs).
- No panics on input-driven paths.
- `Trajectory` retention behind a `Recorder` that no-ops by default in release builds.
- Cancellation/timeouts when LLM-touching paths are added (via `rig-agent` hooks).
- `#![forbid(unsafe_code)]`, declared MSRV (`rust-version` in Cargo.toml).
- Zero dependency on `chatty-eval`, datasets, or statistics crates.
- Semver: public types (`Trajectory`, `Action`, …) are API surface — bump major on breaking changes once stabilized.

## Non-promises

- Does not own the agent loop (chatty-core + rig-agent).
- Does not own Archive (lives in `chatty-optimize`).
