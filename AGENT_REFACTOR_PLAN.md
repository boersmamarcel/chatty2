# AGENT_REFACTOR_PLAN

Phase 1 audit of `boersmamarcel/chatty2` — analysis only, no behavior changes
proposed. The goal is to make this repository easier for AI coding agents to
navigate, understand, and modify safely within limited context windows.

> **Status:** Phase 1 (Audit) complete. Phase 2 (Execute) is **not started**
> and is awaiting approval of this plan.

---

## 1. Architecture snapshot

### 1.1 Workspace layout

Cargo workspace, Rust edition 2024, 9 member crates plus 1 standalone SDK and
2 example WASM modules:

| Crate | Lines (.rs) | Purpose | UI? |
|---|---:|---|---|
| `chatty-core` | ~47,900 | Models, services, tools, settings, repositories, factories, exporters | No (optional `gpui-globals` feature) |
| `chatty-gpui` | ~31,100 | GPUI desktop frontend (binary: `chatty`) | GPUI |
| `chatty-tui` | ~7,100 | Ratatui terminal frontend + headless/pipe modes (binary: `chatty-tui`) | Ratatui |
| `chatty-wasm-runtime` | ~1,250 | Wasmtime runtime for WASM agent modules | No |
| `chatty-module-registry` | ~940 | Module discovery, manifest, lifecycle | No |
| `chatty-protocol-gateway` | ~2,550 | HTTP gateway exposing WASM modules via OpenAI/MCP/A2A | No |
| `chatty-module-sdk` | ~330 | Standalone SDK for `wasm32-wasip2` modules (not in workspace) | No |
| `hive-client` | ~1,730 | Hive registry client (verify/cache) | No |
| `hive-billing-sdk` | ~600 | Billing SDK with separate `Cargo.lock` | No |

Two example modules live under `modules/echo-agent` and `modules/benford-agent`.

### 1.2 Entry points & binaries

- `crates/chatty-gpui/src/main.rs` — desktop app (`chatty`).
- `crates/chatty-tui/src/main.rs` — terminal app (`chatty-tui`), with
  `--headless` and `--pipe` modes.
- Protocol gateway and module registry expose libraries used by the above.

### 1.3 Data flow (already documented well)

`docs/architecture-overview.md` and `CLAUDE.md` describe startup, message
flow, persistence, and the `StreamManager` event topology in detail. These
documents are accurate against the code as of this audit.

Key shared pattern: chatty-core owns all UI-agnostic state. Both frontends
build on the same services. GPUI integration is opt-in via the
`gpui-globals` feature; TUI does not enable it.

### 1.4 Persistence

- JSON files for settings (providers, models, MCP, secrets, training,
  general, execution, token tracking).
- SQLite for conversations.
- Platform-dependent dirs (`dirs` crate): macOS Application Support, Linux
  XDG, Windows AppData.

---

## 2. Build / test / lint / typecheck / run commands

Documented in `README.md` and `CLAUDE.md`, mirrored by `.github/workflows/ci.yml`
(the source of truth for "what passes"):

```bash
cargo build                          # debug
cargo build --release                # release
cargo test --all-features -- --test-threads=1   # CI uses single-thread (note below)
cargo fmt --check
cargo clippy -- -D warnings
cargo build -p chatty-tui            # extra step in CI
./target/debug/chatty-tui --help     # smoke test
```

WASM modules used by integration tests:

```bash
rustup target add wasm32-wasip2
cd modules/echo-agent && cargo build --target wasm32-wasip2 --release \
  && cp target/wasm32-wasip2/release/echo_agent.wasm .
```

Packaging:
- `scripts/package-macos.sh`
- `scripts/package-linux-appimage.sh`
- `scripts/package-windows.ps1`

**Friction observed:**
- The same command list is described in **3 places** (`README.md`,
  `CLAUDE.md`, `.claude/skills/build-and-check/SKILL.md`) and they have
  drifted slightly (the skill omits `--all-features` and `--test-threads=1`
  used by CI; CLAUDE.md and README do too). There is **no Makefile or
  taskfile** for single-command entry points.
- CI runs `cargo test --all-features -- --test-threads=1` because tests
  show intermittent SIGTRAP on GitHub runners when parallel. This is an
  important footgun that is mentioned only in a CI comment.
- Building requires the wasm32-wasip2 target + a separate build of the
  echo-agent WASM before `cargo test` works end-to-end. Not obvious.
- On Linux a substantial system package list is required (documented in
  CLAUDE.md only, not in a setup script).
- This repository is **not built in the audit sandbox** (no warranty here
  about pristine clones building first-try); the audit relies on CI being
  green on `main` as the source of truth.

---

## 3. Test coverage reality

| Crate | Files with tests | Notes |
|---|---:|---|
| chatty-core | 82 | Heavy unit coverage in `tools/`, `services/`, `token_budget/`, `factories/`. One integration file. |
| chatty-gpui | 14 | One integration file (`tests/core_integration.rs`, ~200 lines). Lighter unit coverage. |
| chatty-tui | 5 | Light. No `tests/` dir. |
| chatty-protocol-gateway | 2 | Two integration files (gateway + echo agent e2e). |
| chatty-wasm-runtime | 3 | Unit tests in source. |
| chatty-module-registry | 2 | Unit tests in source. |
| hive-client | 2 | Unit tests in source (cache, verify). |
| hive-billing-sdk | 1 | JWT integration tests. |

~988 `#[test]`/`#[tokio::test]` annotations total across the workspace.

**Reality check:**
- The **largest, riskiest UI/controller files** (`message_ops.rs` 2007,
  `chat_view.rs` 1941, `chat_input.rs` 1689, `conversation_ops.rs` 1157,
  `app_controller/mod.rs` 810) have very little direct test coverage.
  Refactoring them is high-risk without **characterization tests** first.
- Tool tests are good; service tests are decent; controller/view tests are
  thin.
- The serialized (`--test-threads=1`) full suite is the slow path; a "fast
  unit-only" entry point does not exist.

---

## 4. Top friction points for an AI agent

Ranked by how often they will hurt agent productivity / safety.

### 4.1 No `AGENTS.md` at the repo root
There is a strong `CLAUDE.md` (1090 lines) — but tools that look for
`AGENTS.md` (a growing convention) find nothing. `CLAUDE.md` is also long
enough that it nearly fills a small context window on its own; agents may
not load it all.

### 4.2 Oversized files (the biggest single hazard)
13 source files exceed 1000 lines; the top 5:

| File | Lines |
|---|---:|
| `crates/chatty-gpui/src/chatty/controllers/app_controller/message_ops.rs` | 2007 |
| `crates/chatty-gpui/src/chatty/views/chat_view.rs` | 1941 |
| `crates/chatty-gpui/src/chatty/views/chat_input.rs` | 1689 |
| `crates/chatty-gpui/src/auto_updater/mod.rs` | 1522 |
| `crates/chatty-gpui/src/chatty/views/trace_components.rs` | 1453 |
| `crates/chatty-gpui/src/main.rs` | 1451 |
| `crates/chatty-tui/src/headless.rs` | 1401 |
| `crates/chatty-gpui/src/settings/views/models_page.rs` | 1325 |
| `crates/chatty-core/src/tools/data_query_tool.rs` | 1312 |
| `crates/chatty-core/src/exporters/atif_exporter.rs` | 1265 |
| `crates/chatty-core/src/tools/daytona_tool.rs` | 1218 |
| `crates/chatty-core/src/services/shell_service.rs` | 1205 |
| `crates/chatty-core/src/chatty/controllers/app_controller/conversation_ops.rs` | 1157 |

For an agent with a ~50–200k-token window, several of these are too large
to load fully alongside their dependencies and call sites. They are the
files where regressions are most likely.

### 4.3 Re-export confusion in `chatty-gpui`
`crates/chatty-gpui/src/chatty/mod.rs` re-exports
`chatty_core::{auth, exporters, factories, repositories, tools}` as
`crate::chatty::*`. So `use crate::chatty::tools::…` in chatty-gpui actually
refers to chatty-core. This is convenient but confusing: grep for a path
in chatty-gpui's source may not show where the implementation lives.

### 4.4 Three places describe how to build / test
`README.md`, `CLAUDE.md`, and `.claude/skills/build-and-check/SKILL.md`
each have a different command list. There is no single command runner. CI
uses `--test-threads=1` for stability; the other docs do not.

### 4.5 Setup is undocumented as a script
Linux system packages, wasm32-wasip2 target, and the echo-agent WASM
prebuild are all required before `cargo test` works. Currently described
only in prose; no script.

### 4.6 Singletons scattered across modules
`crates/chatty-core/src/lib.rs` has a helpful "Singleton Inventory"
comment, but several singletons still live elsewhere (`GLOBAL_WRITE_APPROVAL_MODE`,
`AZURE_TOKEN_CACHE`, `MCP_WRITE_LOCK`, `PATH_AUGMENTED`,
`OAUTH_CREDENTIAL_REPOSITORY`). The inventory is partial and not visible
from the docs/ tree.

### 4.7 `#[allow(dead_code)]` / `#[allow(unused…)]` count
77 occurrences across the workspace. Some are legitimate (feature-gated
code), but a non-trivial number likely mask refactoring opportunities.
This makes it hard for an agent to trust the "is it used?" signal from
`cargo check`.

### 4.8 Two separate Cargo lockfiles
`Cargo.lock` (workspace) and `crates/hive-billing-sdk/Cargo.lock` (member
crate). The hive-billing-sdk lockfile is in `.gitignore` actually — but
the file structure suggests it can stand alone. Easy to miss when bumping
deps.

### 4.9 Stale repository memories
Memory entries claim "vendor patches removed" while another claims the
workspace patches rig-core via a vendor patch. Verified against
`Cargo.toml`: **no `[patch.crates-io]` section exists**, no `vendor/`
directory exists in the working tree. The "vendor patches removed"
memory is current; the older "actively patches" memory is stale. (Recorded
here so a future agent doesn't waste cycles.)

### 4.10 Minor documentation issues
- `docs/curated-mcp-catalog.md` and `docs/mcp-curated-catalog.md` are two
  separate files with overlapping names — likely one is older.
- `docs/architecture-overview.md` describes 3 crates; the workspace
  actually has 9. The doc is partially out of date with the WASM/module
  expansion.
- README's "Workspace Structure" section lists 7 crates but the
  architecture overview lists 3. Inconsistent.

### 4.11 No fast-feedback test target
Every agent run pays the full `--all-features` workspace compile cost.
There is no `cargo test -p chatty-core --lib` documented entry point for
"I only touched a tool, test the tools quickly."

---

## 5. Prioritized incremental plan

Sorted by **(impact on agent-friendliness ÷ risk)**. Behavior-preserving
items only. Each step is independently verifiable (build + tests + lint
pass between steps).

### Tier 1 — High impact, near-zero risk (recommended for Phase 2)

1. **Add `AGENTS.md` at the repo root.** Short (≤300 lines), pointer-style:
   architecture map (one paragraph + file tree), build/test/lint
   one-liners, conventions (event-emitter, optimistic update, weak
   entities), known gotchas (single-thread tests, gpui-globals feature,
   wasm prebuild), "where things live" cheat sheet, and links into
   `CLAUDE.md` and `docs/` for depth. Symlink or short stub `CLAUDE.md`
   if helpful — but keep both files (some agents read only one). Risk:
   none (docs only).

2. **Add `Makefile` (or `justfile`) with single-command entry points** for
   `setup`, `build`, `test`, `test-fast`, `lint`, `fmt`, `fmt-check`,
   `typecheck` (alias to `cargo check`), `run-gpui`, `run-tui`,
   `wasm-modules`. Use the **CI invocation** (`--all-features
   --test-threads=1`) as canonical so commands match what CI verifies.
   Risk: none (the Makefile only calls existing cargo commands).

3. **Add `scripts/setup-linux.sh`** that installs the apt packages from
   CLAUDE.md and runs `rustup target add wasm32-wasip2`. Document on
   macOS/Windows what is needed. Risk: none.

4. **Fix doc drift on commands.** Reduce the 3 command lists in
   README/CLAUDE/skill to one source plus pointers. The CI workflow is
   the ground truth; AGENTS.md and the Makefile become the second
   surface. Risk: none.

5. **Reconcile architecture docs with current crate count.** Update
   `docs/architecture-overview.md` and README's workspace section to list
   all 9 crates. Note which crates are libraries vs. binaries. Risk: none.

6. **De-duplicate `docs/curated-mcp-catalog.md` vs
   `docs/mcp-curated-catalog.md`.** Keep one; redirect the other or
   delete the older one (verify with `git log`). Risk: none.

7. **Update the "Singleton Inventory" comment in `chatty-core/src/lib.rs`**
   if any singleton has moved since it was written, and link to it from
   AGENTS.md so an agent searching for "is there a global X?" finds it
   in one place. Risk: none.

8. **Clarify the `chatty::*` re-exports.** Add a clear comment block at
   the top of `crates/chatty-gpui/src/chatty/mod.rs` explaining that
   `auth`, `exporters`, `factories`, `repositories`, `tools` are
   re-exports from chatty-core, with a "where is this defined?" pointer.
   Risk: none.

### Tier 2 — High impact, low risk (do if Tier 1 ships cleanly)

9. **Add a per-crate `README.md`** (one paragraph each) to every crate
   that doesn't have one, describing its responsibility and its public
   surface in 1 screen. (`chatty-tui`, `chatty-protocol-gateway`, and
   `hive-billing-sdk` already have READMEs; add for the others.) Risk: none.

10. **Split the docs `docs/INDEX.md`.** A simple table-of-contents file
    so agents can navigate `docs/` without listing it. Risk: none.

11. **Audit and document the `#[allow(dead_code)]` and `#[allow(unused…)]`
    annotations.** For each, either delete the annotation (if no longer
    needed) or add a short comment explaining why it is needed (e.g.,
    "used only behind feature X"). No code logic changes. Risk: very low
    (compile-time only).

12. **Document the "fast unit test" recipe** in AGENTS.md and the
    Makefile: `cargo test -p chatty-core --lib` for tools/services
    changes; `cargo test -p chatty-tui` for TUI changes; etc. No code
    changes. Risk: none.

### Tier 3 — Moderate impact, moderate risk (defer past initial Phase 2)

13. **Add module-header docstrings to the 13 files >1000 lines** explaining
    what the file is responsible for, what it does *not* own, and the
    main types/functions an agent should look at first. No code changes.
    Risk: very low.

14. **Add characterization tests for `message_ops.rs`, `conversation_ops.rs`,
    and the `StreamManager` event handlers** *before* any future split.
    These tests lock in current behavior so a future structural refactor
    is safe. Risk: low (additive only), but **non-trivial work**.

### Tier 4 — High impact, but explicitly DEFERRED (do not do in Phase 2)

These would alter file boundaries or surface area meaningfully. Calling
them out so they are not done implicitly:

- **Splitting `message_ops.rs` (2007 LOC), `chat_view.rs` (1941),
  `chat_input.rs` (1689).** High value for agent comprehension, but
  high risk without characterization tests (see Tier 3, step 14). Defer
  until those tests exist. **Recommendation:** not in this PR.
- **Splitting `auto_updater/mod.rs` (1522 LOC), `data_query_tool.rs` (1312),
  `daytona_tool.rs` (1218), `shell_service.rs` (1205).** Same reasoning —
  defer until tests cover them.
- **Reorganizing the `chatty-gpui::chatty::*` re-exports** to use
  `use chatty_core::…` directly in callers. Touches many files; a Tier 1
  comment block addresses the comprehension issue without churn.
- **Consolidating the partially-duplicated `docs/curated-mcp-catalog.md`
  vs `docs/mcp-curated-catalog.md` content into one well-edited page**
  (beyond simple deletion of the older file).
- **Removing the hive-billing-sdk's separate `Cargo.lock`.** It looks
  intentional (standalone SDK). Leave alone unless owner confirms.
- **Anything that changes a public API or on-disk file format.** Per the
  problem statement constraints.

### Tier 5 — Recommendations (won't act on; out of scope for behavior-preserving work)

- **Re-evaluate the wasm32-wasip2 prebuild dependency for CI tests.**
  The echo-agent WASM is required before tests run. Consider committing
  a pre-built `.wasm` for tests (but that conflicts with current
  `.gitignore`) or providing a `cargo xtask` to do the build. This is a
  build-system choice for maintainers.
- **Reduce overall coupling** of `app_controller` to so many child
  modules; the "fat controller" pattern is documented as intentional in
  `docs/architecture-overview.md`. Worth revisiting at design level.
- **Investigate the `--test-threads=1` SIGTRAP root cause.** Currently a
  workaround; the real fix would speed up CI and any agent that runs the
  full suite.

---

## 6. Proposed Phase 2 scope (concrete)

If approved, Phase 2 will execute Tier 1 (steps 1–8) only, as separate
commits in this order, each verified by build + tests + lint + fmt:

1. `docs: add AGENTS.md and Makefile, link CI-truthful commands`
2. `docs: add scripts/setup-linux.sh and document setup in AGENTS.md`
3. `docs: reconcile crate count in README and docs/architecture-overview.md`
4. `docs: remove duplicate MCP catalog file`
5. `docs: clarify chatty-gpui::chatty re-export block`
6. `docs: update chatty-core singleton inventory comment if drifted`

Tier 2 and 3 deferred to follow-up PRs to keep diffs small and reviewable.

No source code logic changes. No dependency changes. No public API
changes. All commands verified before commit.

---

## 7. Stop here — awaiting approval

Per the problem statement: *"Pause and present this plan before executing."*

This file is the Phase 1 deliverable. Phase 2 has **not started**. Please
confirm scope (Tier 1 as listed above, or a different subset) before I
make any source changes.
