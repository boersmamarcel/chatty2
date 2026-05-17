# Refactor follow-ups (Tier 5)

This document records the open items from the
agent-friendliness refactor that intentionally were **not** completed
in-line, the reason each was deferred, and concrete next steps. Anyone
picking these up should treat the rationale here as the acceptance
criterion — a change that only moves code without addressing the named
risk should not be merged.

The work is grouped by the tier from the original audit. Tier 1–3 and
the safe portion of Tier 4 have already shipped on this branch; the
items below are what remains.

---

## 1. Oversized files — splits completed and remaining

Tier 4 of the audit identified ~10 files above the 1000-LOC guideline.
The visual / UI files where the user reported real bugs have now been
split. Several non-UI files remain deferred because they share a common
risk profile that the audit itself flagged: **test coverage is too thin
to catch behavioural regressions introduced by a mechanical split**.

### Completed on this branch

| File | Before | After (`mod.rs`) | Split into |
|---|---:|---:|---|
| `chat_view.rs` | 1966 | 852 | `chat_view/{handlers, sub_agent, history, start_screen}.rs` |
| `chat_input.rs` | 1711 | 415 | `chat_input/{render, slash, at_mention}.rs` |
| `trace_components.rs` | 1474 | 147 | `trace_components/{badges, blocks, inline}.rs` |
| `app_controller.rs` (Tier 4 earlier) | — | — | `app_controller/{message_ops, conversation_ops, …}.rs` |

The pattern used in all three: a `*/mod.rs` retains the struct
definition, lifecycle, and (for UI) the `Render` impl; sibling modules
own one category of behaviour each (event handling, history loading,
sub-views, sub-pickers). Cross-module method calls use `pub(super)`,
which surfaces the seam-crossing dependency at the import site instead
of hiding it inside a 1700-line scope.

### Files still pending splits

| File | LOC | Why a split is risky today |
|---|---:|---|
| `crates/chatty-gpui/src/settings/views/models_page.rs` | ~1400 | Mixed form state + provider-specific UI; safer to extract per-provider sections only after the provider abstraction in `ProviderType::default_capabilities` covers more of the form logic. |
| `crates/chatty-core/src/tools/data_query_tool.rs` | 1332 | Single tool dispatcher with many ad-hoc parsing branches; tests cover happy paths but not the dispatch table. |
| `crates/chatty-core/src/tools/daytona_tool.rs` | 1239 | Talks to an external Daytona sandbox; HTTP error-handling branches are not mocked. |
| `crates/chatty-core/src/services/shell_service.rs` | 1225 | PTY allocation + signal handling + approval flow + output capping. Splitting without race tests is unsafe. |
| `crates/chatty-core/src/exporters/atif_exporter.rs` | 1283 | Format spec is in code; the file *is* the spec. Tests cover the round-trip but not byte-level layout. |
| `crates/chatty-gpui/src/auto_updater/mod.rs` | 1522 | Update lifecycle across three OSes; the platform `#[cfg]` branches make a flat file easier to reason about than a multi-file split until per-OS golden tests exist. |
| `crates/chatty-gpui/src/main.rs` | 1471 | Mostly action / keybinding / startup wiring; clean seams exist but the file is essentially one long startup script. A split would primarily move boilerplate and provides little context-window relief during edits because callers usually open the whole file. |
| `crates/chatty-tui/src/headless.rs` | 1424 | 50+ private helpers around stream-recovery heuristics. The helpers are individually trivial but their *ordering* matters for benchmark stability; splitting without characterization tests on full benchmark runs risks subtle scoring regressions. |

### Recommended next steps for each file

The audit-aligned sequence for any of the files above is:

1. **Add characterization tests first.** Pick the file. Run it through
   its current happy paths (and at least two error paths) and assert on
   *observable* outputs — rendered text, tool-call JSON, exporter
   bytes, exit codes. The point is not to test correctness of the new
   behaviour, only to **fingerprint the current behaviour** so the
   split can be detected if it drifts.
2. **Identify the seam.** Most of these files have an obvious
   responsibility boundary (e.g. `chat_input.rs` → composition vs.
   attachments vs. slash-commands vs. model picker). Pick the *single*
   boundary with the cleanest interface and split only that one.
   Resist the urge to split everything at once.
3. **Move definitions, not call sites.** Keep the public surface of
   the original file unchanged. Move helpers and private types into a
   sibling `*_internals.rs` (the pattern established by Tier 4 for
   `message_ops`).
4. **Re-run the characterization tests.** No drift = merge. Drift =
   revert the split, fix the test, try again.
5. **Update the module-level docstring at the top of the original
   file** to point at the new sibling — these docstrings are how an
   agent finds the file in the first place.

For `main.rs` and `auto_updater/mod.rs` specifically, an alternative
to splitting is to add a top-of-file **table of contents** comment
that lists the section headers (`// === keybindings ===`,
`// === window setup ===`, etc.) with line ranges. That gives the
same navigability benefit without the platform-`#[cfg]` headache.

---

## 2. Tier-5 recommendations (process / infrastructure)

These were flagged in the original audit as one-off improvements that
don't fit into a "split a file" workflow. None require code changes
yet — they need an owner decision before any implementation.

### 2a. WASM module prebuild redesign

**Current state.** `make wasm-modules` is required before
`cargo test --all-features` will pass, because three integration tests
load `modules/echo-agent/echo_agent.wasm` from disk. The build target
is `wasm32-wasip2`, which is not the host target and is not built by
`cargo test` automatically. New contributors hit a confusing
"No such file or directory" failure if they skip the make step.

**Options:**

1. **Status quo + better error message.** Wrap the `include_bytes!` /
   file-open with a clear "Run `make wasm-modules` first; see
   AGENTS.md" message. Lowest risk; documents the footgun in-place.
2. **`build.rs` in the test crate** that shells out to `cargo build
   --target wasm32-wasip2 --release -p echo-agent` when the artifact
   is missing. Removes the manual step but adds a build-script
   dependency on `cargo` and on the `wasm32-wasip2` target being
   installed (which it isn't by default).
3. **Vendor a tiny prebuilt wasm fixture** under `tests/fixtures/`
   that exercises the same WIT contract. Decouples the integration
   tests from `modules/echo-agent` entirely. Best long-term option;
   highest one-time effort.

**Recommendation:** start with option 1 (one-line fix), then evaluate
option 3 if the prebuild step continues to cause test failures.

### 2b. `--test-threads=1` SIGTRAP investigation

**Current state.** `.github/workflows/ci.yml` runs
`cargo test --all-features -- --test-threads=1` because parallel test
execution intermittently SIGTRAPs `chatty-core` on GitHub-hosted
runners. The workaround is documented in CI, but the root cause is
unknown. Single-threaded tests roughly triple wall time on the slowest
crates.

**Investigation plan:**

1. **Reproduce locally** under
   `RUST_TEST_THREADS=8 RUST_BACKTRACE=full cargo test -p chatty-core`
   in a loop (e.g. 50 iterations). Likely the bug only reproduces on
   GitHub's specific glibc + kernel combination; capturing a core
   dump there requires enabling `ulimit -c unlimited` and uploading
   the dump as an artifact.
2. **Bisect by test module.** Disable test modules in halves until the
   SIGTRAP no longer reproduces. The most likely culprits are tests
   that touch the global tracing subscriber, the global Tokio runtime,
   or anything that calls `LazyLock::force()` from multiple threads
   simultaneously — known sources of init-race SIGTRAPs in Rust on
   musl/glibc combinations.
3. **Check `unsafe` blocks** in `chatty-core` that touch
   process-global state (signal handlers, env, fds). The singleton
   inventory at the top of `crates/chatty-core/src/lib.rs` is the
   right starting list.
4. **Run with ASan.** `RUSTFLAGS="-Z sanitizer=address"
   cargo +nightly test` may catch a use-after-free or double-free in
   the FFI bindings (rig-core ↔ tokenizers ↔ Typst).

**Effort estimate:** a 1–2 day investigation. The fix is likely small
once the cause is identified; the cost is the bisection.

### 2c. Fat-controller decoupling

**Current state.** `ChattyApp` (in
`crates/chatty-gpui/src/chatty/controllers/app_controller/`) is the
god-object of the desktop UI. Even after Tier 4's split into
`message_ops` / `message_ops_internals` /
`conversation_ops` / `conversation_ops_modify` /
`export_ops` / `slash_commands`, every method still calls into
`ChattyApp` and mutates its full state. The split made navigation
easier but did not reduce coupling.

**The architectural direction** (not a single PR's worth of work) is
to **push behaviour into the entities `ChattyApp` already owns** so
that `ChattyApp` becomes a thin dispatcher:

| Today: `ChattyApp` does this directly | After: delegated to |
|---|---|
| Manages chat view state and rendering | `ChatView` (already an entity) |
| Manages stream lifecycle | `StreamManager` (already an entity) |
| Manages conversation list and active conversation | `ConversationsStore` global + `SidebarView` |
| Persists conversations | `ConversationRepository` (already exists) |
| Routes slash commands | A new `SlashCommandDispatcher` entity |
| Holds the `AgentClient` | `Conversation` itself, or a per-conversation `AgentSession` entity |

The migration path is **per-method, not big-bang.** Each method on
`ChattyApp` that mutates only one field can be moved to that field's
owning entity and re-implemented as an event the owner subscribes to.
The decoupling is complete when `ChattyApp::update` no longer needs
to touch more than one sibling entity per call.

This is intentionally framed as a direction rather than a task: doing
it requires per-method judgement and code review; doing it
mechanically would just shuffle the coupling without removing it. See
`docs/entity-communication.md` for the event-based pattern the
migration should follow.

### 2d. Re-export removal follow-ups

Tier 4 removed the `pub use chatty_core::{auth, exporters, factories,
repositories, tools}` re-exports from `crates/chatty-gpui/src/chatty/mod.rs`,
so call sites now import from `chatty_core::…` directly. Two
follow-ups remain:

1. **Lint rule.** Add a clippy or
   `[lints.rust] unused_imports = "deny"`-style guard to prevent
   re-introduction of the re-exports. The natural place is
   `crates/chatty-gpui/src/chatty/mod.rs` itself, where a comment
   already calls out the convention.
2. **chatty-tui consistency check.** Audit `chatty-tui` for the same
   re-export anti-pattern; the audit only confirmed `chatty-gpui` was
   the source of the wildcard re-exports.

---

## 3. What this branch *did* land (for reference)

So the open items above are easier to scope, here is what is already
in place on this refactor branch:

- **Tier 1.** Deleted obsolete `vendor/esaxx-rs` (per the audit:
  duplicate of crates.io 0.1.10); kept the active rig-core patch.
  Consolidated `docs/curated-mcp-catalog.md` and
  `docs/mcp-curated-catalog.md` into one file. Tightened the
  copilot-setup steps.
- **Tier 2.** Added per-crate README pointers, root `docs/INDEX.md`,
  fast-test recipes in `Makefile` (`make test-fast`, `test-tui`,
  `test-gpui`, `test-gateway`), and the wasm-modules / single-thread
  footgun notes to `AGENTS.md`.
- **Tier 3.** Added module-header docstrings (`//!` blocks) to every
  file over 1000 LOC explaining "what lives here" and "what does NOT
  live here", so agents can scope a file from the top without
  reading the whole thing.
- **Tier 4 (partial).**
  - Removed the `chatty/mod.rs` re-export wildcards; updated all call
    sites in `chatty-gpui` to import from `chatty_core` directly.
  - Split `message_ops.rs` (2036 → 1259 LOC) by extracting
    `LlmStreamParams`, `run_llm_stream`,
    `attachment_to_user_content`, `select_recent_assistant_attachments`,
    `is_auth_stream_error`, `should_refresh_azure_auth`, and their
    tests into `message_ops_internals.rs`.
  - Split `conversation_ops.rs` (1177 → 702 LOC) by moving runtime
    modification methods (navigate, start_new, delete_active,
    rebuild_active_agent, change_conversation_{model,working_dir},
    delete_conversation, persist_conversation) into
    `conversation_ops_modify.rs`.
  - Validated by `cargo check --all-features` and the full
    `cargo test -p chatty-gpui --tests` suite (119/119 passing).
- **Tier 5 (this document).** Written as guidance for the remaining
  work above.
