# Test

**When to read this:** You want to run the right part of the suite for the change you made, understand why CI runs tests the way it does, or add a test in the place the next reader will look.

## Goal

Know which `make` target or `cargo test` invocation covers your change, and be able to update a characterization golden deliberately rather than by accident.

## Prerequisites

- [Build and run](../start/build-and-run.md): `make setup` done.
- `make wasm-modules` has been run. The gateway integration tests load `modules/echo-agent/echo_agent.wasm`, which is git-ignored; without it they fail with a missing-file error.

## How the suite is organised

| Where | What it covers | Run with |
|-------|----------------|----------|
| `#[cfg(test)] mod tests` beside the code, in every crate | Unit tests. Most logic is in `chatty-core`, so most tests are too | `make test-fast` (`cargo test -p chatty-core --lib`), or `cargo test -p <crate>` |
| [`crates/chatty-core/tests/integration.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/tests/integration.rs) | The public API from an external consumer's point of view: settings model CRUD lifecycle, `ConversationsStore` operations and ordering, provider capability propagation into `ModelConfig`, Azure provider configuration filtering, JSON serialization round-trips, token budget snapshot calculations. No display server | `cargo test -p chatty-core --test integration` |
| [`crates/chatty-core/tests/browser_live.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/tests/browser_live.rs) | End to end against a real Chrome, `#[ignore]`d because the first run may download the pinned build | `cargo test -p chatty-core --features browser --test browser_live -- --ignored --test-threads=1` |
| [`crates/chatty-gpui/tests/core_integration.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/tests/core_integration.rs) | chatty-core types compiled with the `gpui-globals` feature: every `impl Global` still behaves, cross-crate interactions, token budget snapshots. Links GPUI but needs no live display | `make test-gpui` |
| [`crates/chatty-protocol-gateway/tests/`](https://github.com/boersmamarcel/chatty2/tree/main/crates/chatty-protocol-gateway/tests) (`gateway_tests.rs`, `echo_agent_e2e.rs`) | The three gateway protocols against the echo-agent module | `make test-gateway` |
| `gemini_compat_tests` in [`crates/chatty-core/src/tools/mod.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/tools/mod.rs) | Every tool schema converts to a Gemini `Schema` without empty `type` strings | `cargo test -p chatty-core --lib gemini_compat` |
| Characterization goldens (three directories, below) | The event sequence each frontend produces for a scripted stream | see below |
| `crates/hive-billing-sdk/tests/` | Standalone SDK with its own `Cargo.lock`; not a workspace member | `cd crates/hive-billing-sdk && cargo test` |

## Steps

### 1. Inner loop

```bash
make test-fast                    # cargo test -p chatty-core --lib
cargo test -p chatty-core --lib my_module::   # one module
```

### 2. The crate you touched

```bash
make test-tui       # cargo test -p chatty-tui
make test-gpui      # cargo test -p chatty-gpui
make test-gateway   # cargo test -p chatty-protocol-gateway
```

### 3. Everything, the way CI runs it

```bash
make test           # cargo test --all-features -- --test-threads=1
```

`--all-features` matters: the `excel`, `docx`, `pdf`, `pptx`, `math-render`, `mermaid`, `duckdb` and `browser` tool groups only compile with their feature on, so a plain `cargo test` never sees them. `make ci` adds formatting, clippy and the repository check scripts; see [Make targets & CI workflows](../ci-reference.md).

> [!WARNING]
> **`--test-threads=1` is not optional in CI.** `chatty-core` tests intermittently die with SIGTRAP under parallel execution on GitHub-hosted runners; the root cause is unknown and the serialisation is the documented workaround in `.github/workflows/ci.yml`. If CI shows a SIGTRAP and the same tests pass for you, run `cargo test --all-features -- --test-threads=1` locally to reproduce before assuming flakiness.

### 4. Characterization goldens

Turn orchestration — which chunk produces which event, which chunks end a turn, that cancellation produces a cancelled outcome, that sub-agent progress is drained ahead of stream chunks — is pinned by golden files rather than by assertions scattered across tests.

The scripted streams live in [`chatty_core::services::stream_fixtures`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/services/stream_fixtures.rs) (`scenarios()`, `clarification_scenario()`, `scripted_stream`, `assert_golden`). The module is compiled under `#[cfg(any(test, feature = "test-support"))]`; `chatty-tui` and `chatty-gpui` enable `chatty-core/test-support` from their dev-dependencies only, so it never reaches a release build.

Both frontends and the core loop drive the **same** scenarios and record their own event type, so a divergence between the two UIs shows up as a difference between two golden directories:

| Golden directory | Recorded by | Event type |
|------------------|-------------|------------|
| `crates/chatty-core/src/services/goldens/stream_loop/` | `loop_callback_sequence_matches_goldens` in [`stream_processor.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/services/stream_processor.rs) | `StreamChunkHandler` callbacks |
| `crates/chatty-core/src/session/goldens/` | [`session/tests.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/session/tests.rs) | `SessionEvent` |
| `crates/chatty-tui/src/engine/goldens/` | [`engine/characterization.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-tui/src/engine/characterization.rs) | `AppEvent` |
| `crates/chatty-gpui/src/chatty/controllers/app_controller/goldens/` | [`session_characterization.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-gpui/src/chatty/controllers/app_controller/session_characterization.rs) | `StreamManagerEvent` |

Each directory holds one file per scenario: `text_only`, `tool_call_then_result`, `tool_error`, `approval_granted`, `approval_denied`, `clarification_requested`, `provider_error_mid_stream`, `cancelled_mid_stream`, `sub_agent_progress`, `token_usage_on_done`.

When a golden fails, the assertion prints both sequences. If the change is deliberate, rewrite the files and explain the diff in the PR:

```bash
UPDATE_GOLDENS=1 cargo test -p chatty-core --lib loop_callback_sequence
UPDATE_GOLDENS=1 cargo test -p chatty-core session::
UPDATE_GOLDENS=1 cargo test -p chatty-tui characterization
UPDATE_GOLDENS=1 cargo test -p chatty-gpui characterization
```

Deliberately *not* pinned: the real-time interleaving of progress events with chunks (racy in production, so scenarios queue progress before the chunks it accompanies), and anything wall-clock based — the stall watchdog has its own tests in `stream_processor.rs`.

### 5. Where mocks live

- [`crates/chatty-core/src/tools/test_helpers.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/tools/test_helpers.rs) — `MockMcpRepository` and `MockA2aRepository`, in-memory repositories with injectable load/save errors, `#[cfg(test)]` only. Used by the MCP and A2A tool tests.
- Providers are not mocked; the scripted streams above stand in for a provider whenever a test needs "what the model sent".
- The desktop characterization test talks to real entities (a `ChatView` in a headless test window, a live `StreamManager`), not stubs, so what it records is what the UI receives.
- `browser_live.rs` builds a throwaway workspace with `tempfile` rather than fixtures on disk.

## Verify

- `make test-fast` is green after a core change; the crate target is green after a frontend change.
- `make test` (full, serialised) is green before you push.
- A golden you changed on purpose has its diff explained in the PR.

## Checklist

- [ ] `make wasm-modules` has been run on this checkout
- [ ] New logic in `chatty-core` has a unit test beside it
- [ ] A new tool has a `gemini_compat` guard (see [Your first change](../start/first-change.md))
- [ ] Turn-lifecycle changes re-recorded all three golden directories, not one
- [ ] `make test` passes with `--test-threads=1`

## Common mistakes

| Mistake | Do this instead |
|---------|-----------------|
| Deleting a golden file to make a test pass | `UPDATE_GOLDENS=1` and explain the diff |
| Updating the TUI goldens but not the desktop ones (or the reverse) | Re-run all three; the point is that they agree |
| `cargo test` without `--all-features` before pushing | `make test` |
| Reproducing a CI SIGTRAP with parallel tests | `--test-threads=1` |
| Running `browser_live` in CI | It is `#[ignore]`d on purpose; run it locally |
| Enabling `test-support` in a normal `[dependencies]` block | Dev-dependency only |
