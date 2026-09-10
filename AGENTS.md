# AGENTS.md

<!-- DO NOT REMOVE: ownership contract pointer. Regenerating this file must preserve it. -->
> Research crates carry reserved symbols; read [`RESERVED.md`](RESERVED.md) before touching
> them. It also holds the issue-label and auto-ship rules. Ordinary chatty2 work is unaffected.

Quick-start guide for AI coding agents working in this repository.
Optimized for limited context windows: read this first, then dive deeper
via the links below.

For human-oriented documentation, see the [user guides](docs-site/src/user/getting-started.md)
(published at <https://boersmamarcel.github.io/chatty2/>). The repo
[`README.md`](README.md) is a short landing page (download, links, dev
quick-start). For coding patterns see [`CLAUDE.md`](CLAUDE.md). For
architecture, see [`docs/INDEX.md`](docs/INDEX.md).

---

## What is this repo?

**Chatty** — a desktop and terminal AI agent built in Rust. Two binaries
(GPUI desktop app, Ratatui terminal app) backed by a shared, UI-agnostic
core crate.

## Workspace map

```
crates/
├── chatty-core/              # UI-agnostic: models, services, tools, settings,
│                             #   repositories, factories, exporters
├── chatty-gpui/              # Desktop binary `chatty` (GPUI)
├── chatty-tui/               # Terminal binary `chatty-tui` (Ratatui)
│                             #   also: --headless and --pipe modes
├── chatty-wasm-runtime/      # Wasmtime runtime for WASM agent modules
├── chatty-module-registry/   # Module discovery, manifest, lifecycle
├── chatty-protocol-gateway/  # HTTP gateway: OpenAI / MCP / A2A protocols
├── chatty-module-sdk/        # SDK for `wasm32-wasip2` modules (standalone)
├── chatty-trace/             # Research: traces / ATIF / FeedbackFn (AGE-5, ships)
├── chatty-playbook/          # Research: ACE playbook (AGE-17, ships)
├── chatty-flow/              # Research: AFlow WorkflowRepr + IR (AGE-13, ships)
├── chatty-optimize/          # Research: GEPA/AFlow + paired stats + QA loaders
├── hive-client/              # Hive registry client
└── hive-billing-sdk/         # Billing SDK (separate Cargo.lock)

modules/                      # Reference WASM agent modules (echo, benford)
docs/                         # Deep-dive architecture and design docs
scripts/                      # Packaging + setup scripts
```

**Dependency direction:** `chatty-gpui` and `chatty-tui` depend on
`chatty-core`. `chatty-core` never depends on a UI crate. GPUI's `Global`
trait impls live behind the `gpui-globals` feature.

## Where things live (cheat sheet)

| You want to… | Look here |
|---|---|
| Add an LLM provider | `crates/chatty-core/src/factories/agent_factory/` + `ProviderType` |
| Add an LLM tool | `crates/chatty-core/src/tools/`, register in `agent_factory.rs` |
| Add a service | UI-agnostic → `crates/chatty-core/src/services/`. UI-specific → `crates/chatty-gpui/src/chatty/services/` |
| Add a setting | model in `chatty-core/src/settings/models/`, repo in `chatty-core/src/settings/repositories/`, UI in `chatty-gpui/src/settings/views/` |
| Add a desktop view | `crates/chatty-gpui/src/chatty/views/` — emit events to `ChattyApp` |
| Add a terminal view | `crates/chatty-tui/src/ui/` |
| Add a workspace-global singleton | Define in `chatty-core`, add `impl Global` in `chatty-core/src/gpui_globals.rs` |
| Find all process-wide statics | Singleton inventory at top of `crates/chatty-core/src/lib.rs` |
| Persist data | JSON repo in `chatty-core/src/settings/repositories/`, or SQLite in `chatty-core/src/repositories/` |
| Add a slash command | `crates/chatty-gpui/src/chatty/controllers/app_controller/slash_commands.rs` |

## Build / test / lint — single source of truth

**The CI workflow** ([`.github/workflows/ci.yml`](.github/workflows/ci.yml))
**is the ground truth.** If a command is not here, it is not what CI runs.

Docs-only PRs (and other diffs that do not match the Rust path filter in
`ci.yml`) skip compile, test, and clippy. The required `test` check still
goes green. Stale CI runs on the same PR are cancelled on the next push.

```bash
make setup            # one-time: install Linux deps + wasm32-wasip2 target
make build            # cargo build (debug)
make test             # cargo test --all-features -- --test-threads=1  (matches CI)
make test-fast        # cargo test -p chatty-core --lib  (quick inner loop)
make test-tui         # cargo test -p chatty-tui          (TUI changes only)
make test-gpui        # cargo test -p chatty-gpui         (GPUI changes only)
make test-gateway     # cargo test -p chatty-protocol-gateway  (gateway changes only)
make lint             # cargo clippy --all-features -- -D warnings (CI also passes --all-targets, see below)
make fmt              # cargo fmt
make fmt-check        # cargo fmt --check
make wasm-modules     # build the echo-agent WASM module (needed by tests)
make docs-gen         # regenerate docs/generated reference pages
make docs             # sync + build mdBook site (docs-site/book/)
make docs-serve       # local preview at http://localhost:3000
make docs-check-links # lychee link check, sources + built site (AGE-117)
make docs-check-nav   # INDEX.md + SUMMARY.md drift check (AGE-116)
make docs-check-frontmatter  # optional YAML frontmatter schema (AGE-115)
make docs-check-leakage      # user guides must not carry contributor material
make docs-check-reference    # reference tables match tool_registry.rs and friends
make docs-check       # all of the docs checks above
make animations       # re-record README/docs GIFs (scripts/animations/README.md)
make ci               # everything the Rust CI path runs, locally, in order
```

Or use cargo directly:

```bash
cargo build
cargo test --all-features -- --test-threads=1
cargo fmt --check
cargo clippy --all-features --all-targets -- -D warnings
```

### Test-thread footgun

CI runs tests with `--test-threads=1` because `chatty-core` tests
intermittently SIGTRAP under parallel execution on GitHub-hosted runners.
**If you see a SIGTRAP in CI but tests pass locally, run with
`--test-threads=1` locally to reproduce.** Root cause is unknown; the
workaround is documented in `.github/workflows/ci.yml`.

### Windows footgun

`ci.yml` never compiles for Windows on a PR — `windows-latest` only runs in
`warm-release-cache`, which is gated `if: push && ref == main` (a cache
warmer, not a required check). A Windows-only compile error (e.g. code that
should be `#[cfg(unix)]`, like the broker/participant modules, AGE-339) rides
straight to `main` and isn't caught until `build-windows` fails during a
release, shipping an assetless tag. If you touch platform-conditional code,
don't trust a green PR as proof Windows still builds.

### Disk footgun

A full `cargo test --all-features` needs about 16 GiB of `target/` even with
the workspace's trimmed dependency debuginfo (`[profile.dev.package.*]` in
the root `Cargo.toml`); at Cargo's defaults it needs 28+ GiB and can run a
small disk out of space mid-link. See
[`docs/build-disk-usage.md`](docs/build-disk-usage.md) before building on a
constrained sandbox or CI runner.

### WASM module prebuild

Some integration tests load `modules/echo-agent/echo_agent.wasm`. Build
it (once) before running the full test suite:

```bash
make wasm-modules
# or:
rustup target add wasm32-wasip2
cd modules/echo-agent && cargo build --target wasm32-wasip2 --release \
  && cp target/wasm32-wasip2/release/echo_agent.wasm .
```

## Running

```bash
cargo run -p chatty-gpui              # desktop app
cargo run -p chatty-tui               # terminal app
cargo run -p chatty-tui -- --help     # CLI options (headless / pipe / direct providers)
```

## Conventions to follow

These are the patterns the codebase already uses. Follow them; don't
invent new ones. See [`CLAUDE.md`](CLAUDE.md) for full rationale and
examples.

- **Event-driven entity communication** — Use `EventEmitter` +
  `cx.subscribe()`. No `Arc<dyn Fn>` callbacks between entities.
  See [`docs/entity-communication.md`](docs/entity-communication.md).
- **Optimistic updates** — Update the in-memory global immediately, then
  persist asynchronously with logged errors.
- **Entity refs in globals** — Default to `GlobalWeakEntity<T>` (avoids
  circular ownership); use `GlobalStrongEntity<T>` only when the global
  must keep the entity alive itself (e.g. `StreamManager`,
  `ModelsNotifier`).
- **Stream lifecycle** — All LLM streams go through `StreamManager` with
  cancellation tokens; the stream loop never updates UI directly, it
  emits events. Streams carry a monotonic epoch so a stale `StreamEnded`
  can't tear down a newer turn, and a shared stall watchdog
  (`chatty-core/src/services/stream_processor.rs`) ends a turn after 180s
  of silence. See [`docs/stream-manager.md`](docs/stream-manager.md).
- **Error handling** — Don't `.ok()` away errors silently. Log as
  `warn!()` for non-critical paths; propagate with `?` for critical I/O.
- **Tool errors** — Every `impl Tool`'s error path must go through
  `map_tool_error(tool_name, error)` (`chatty-core/src/tools/mod.rs`), not
  rig's default `Tool::map_error`, or the model/transcript only ever see
  the redacted string `"the tool failed"`. See "Tool Error Reporting
  Pattern" in CLAUDE.md.
- **MCP API keys** — When sending MCP config to the LLM, report
  `has_api_key()`, never the `api_key` field; a `"****"`
  (`MASKED_API_KEY_SENTINEL`) sent back by the model means "keep the stored
  value". See "Security Practices" in CLAUDE.md.
- **Rust edition** — 2024. Use `LazyLock`/`OnceLock` (std) rather than
  `lazy_static`/`once_cell`.
- **GPUI / gpui-component skills** — When changing desktop UI, load
  `.claude/skills/gpui` and `.claude/skills/gpui-component` (vendored from
  `npx skills add longbridge/gpui-component`; lockfile `skills-lock.json`).
- **Transcript blocks** — Typed block/turn types in
  `chatty-gpui/src/chatty/views/transcript/` render the transcript;
  persistence stays untyped (`MessageEntry` + `system_trace` JSON) in
  chatty-core. Don't leak transcript block types into chatty-core. The
  transcript list uses gpui's `list`/`ListState` (measured heights), not
  `v_virtual_list` (predicted heights) — see CLAUDE.md.
- **Tool failure signal is text, not a flag** — `map_tool_error()`'s
  `Error: {tool_name}: {message}` prefix is the only thing that tells
  `llm_service::tool_result_looks_like_error` a tool call failed. Don't
  drop or reword that prefix. See CLAUDE.md.
- **Stale docs** — If a change alters a fact a page claims, update that
  page in the same PR. `update-agent-docs.yml` only safety-nets
  `AGENTS.md` / `CLAUDE.md`. See the Documentation section of
  [`CONTRIBUTING.md`](CONTRIBUTING.md#documentation).

## Known gotchas

1. **No `chatty-gpui::chatty::*` re-exports.** `crates/chatty-gpui/src/chatty/mod.rs`
   used to re-export `auth`, `exporters`, `factories`, `repositories`, `tools`
   from `chatty_core`; those re-exports were removed so call sites import
   `chatty_core::…` directly. If grep finds no definition under
   `chatty-gpui/`, look in `chatty-core/`.
   `scripts/check-no-core-reexports.sh` (in CI) fails the build if they
   come back.

2. **Test parallelism.** See "Test-thread footgun" above.

3. **WASM module prebuild.** Tests fail with a missing-file error if you
   haven't run `make wasm-modules` first.

4. **Linux system packages.** GPUI needs a long list of `lib*-dev`
   packages. Run `make setup` (or `scripts/setup-linux.sh`) on a fresh
   machine.

5. **Two Cargo lockfiles.** `crates/hive-billing-sdk/` has its own
   `Cargo.lock` (intentional — it's a standalone SDK). When bumping its
   deps, do so in that lockfile too.

6. **The `gpui-globals` feature.** chatty-core types implement
   `gpui::Global` only when this feature is enabled. chatty-gpui enables
   it; chatty-tui does not. If a `Global` impl is missing, add it in
   `crates/chatty-core/src/gpui_globals.rs`.

7. **Sub-agent worktrees.** `sub_agent` tool workers each get their own
   `git worktree` under `<workspace>/.chatty/worktrees/<name>` on a
   `sub-agent/<name>` branch (AGE-314), passed to the child via chatty-tui's
   `--workspace <DIR>` flag. Worktrees are left in place after a worker
   exits (never auto-removed) and are excluded via `.git/info/exclude`, not
   `.gitignore` — they won't show in `git status` but can still accumulate
   on disk. Falls back to the old shared-tree behavior if the workspace
   isn't a git repo. See CLAUDE.md.

8. **Large module directories.** Several complex areas have been split
   into sub-module directories (`chat_view/`, `chat_input/`,
   `auto_updater/`, `trace_components/`, `transcript/`, etc.). Start
   with the `mod.rs` and its module-level docstring to scope what you
   need before loading sibling files. The largest single files are
   `message_ops.rs` (~1260 lines) and `main.rs` (~1225 lines).

## Deeper reading

| Topic | File |
|---|---|
| **System overview (diagrams)** | [`docs/system-overview.md`](docs/system-overview.md) |
| **Component map (diagrams)** | [`docs/component-map.md`](docs/component-map.md) |
| Crate split rationale | [`docs/workspace-crate-split.md`](docs/workspace-crate-split.md) |
| Stream lifecycle | [`docs/stream-manager.md`](docs/stream-manager.md) |
| Entity communication | [`docs/entity-communication.md`](docs/entity-communication.md) |
| Rendering pipeline | [`docs/rendering-system.md`](docs/rendering-system.md) |
| Token budget | [`docs/token-tracking.md`](docs/token-tracking.md) |
| Agent memory | [`docs/agent-memory.md`](docs/agent-memory.md) |
| WASM modules & A2A | [`docs/a2a-and-wasm-modules.md`](docs/a2a-and-wasm-modules.md) |
| WIT reference | [`docs/wit-reference.md`](docs/wit-reference.md) |
| Debugging the UI and streams | [`docs-site/src/dev/guides/debug.md`](docs-site/src/dev/guides/debug.md) |
| Release process | [`docs/RELEASE_PROCESS.md`](docs/RELEASE_PROCESS.md) |
| Coding patterns & behavior | [`CLAUDE.md`](CLAUDE.md) |

## Cloud VM notes

Build/test/lint/run commands are unchanged — use the `make` targets above.
These are only the non-obvious caveats of a headless cloud build VM.

- **Toolchain.** The workspace is edition 2024 and declares
  `rust-version = "1.94"` in the root `Cargo.toml` (the research crates
  inherit it). If a build fails with `feature edition2024 is required`, the
  VM's default toolchain is too old: run `rustup default stable`. CI uses
  `dtolnay/rust-toolchain@stable`, so it always runs ahead of the declared
  MSRV; if a dependency raises the floor, bump `rust-version` and name the
  dependency in the commit.

- **Use GNU `cc`/`c++`, not clang.** The bundled DuckDB C++ build and the
  desktop binary link fail under the system clang (`fatal error: 'memory'
  file not found`, `unable to find library -lstdc++`). If you see either,
  run `sudo update-alternatives --set cc /usr/bin/gcc` and
  `sudo update-alternatives --set c++ /usr/bin/g++`.

- **`modules/echo-agent/echo_agent.wasm` is git-ignored** and required by
  some integration tests; run `make wasm-modules` after a fresh checkout.

- **Disk.** A full `--all-features` test build needs about 16 GiB of
  `target/`; see [`docs/build-disk-usage.md`](docs/build-disk-usage.md)
  before running it on a small VM.

- **Running the desktop app headlessly.** The VM has no GPU but software
  Vulkan (Mesa `llvmpipe`/lavapipe) works on the existing X11 display
  (`DISPLAY=:1`). Export `XDG_RUNTIME_DIR` first, e.g.
  `export XDG_RUNTIME_DIR=/tmp/xdg-runtime-$(id -u) && mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"`,
  otherwise X11 window creation fails with `BadMatch`. `chatty-tui` needs
  none of this.

- **No LLM API key is configured.** To chat from either frontend, use a
  local Ollama: `ollama serve` (systemd is not running, so launch it in a
  background/tmux session), `ollama pull qwen2.5:0.5b`, then
  `./target/debug/chatty-tui --ollama http://localhost:11434 --model qwen2.5:0.5b --headless -m "..."`.
  The desktop app auto-detects a running local Ollama and lists its models
  with no configuration.

- **Clippy is clean workspace-wide.** `cargo clippy --workspace --all-features --all-targets -- -D warnings`
  passes (AGE-174). CI added `--all-targets` so tests/benches/examples are
  linted too — without it, lints inside `tests/` go unreported (a finding sat
  unnoticed in `chatty-protocol-gateway`'s e2e test until this was added).
  `make lint`/`make ci` still invoke the pre-`--all-targets` command, so a
  clean `make lint` no longer guarantees a clean CI clippy; pass
  `--all-targets` yourself to match CI exactly.

- **Local rustc lints strictly less than CI's.** This VM's default toolchain
  (1.94.1) can be behind the `stable` CI uses (e.g. 1.98.1) — clippy findings
  visible only on newer stable (such as redundant glob imports) can pass
  locally and fail CI. Run `rustup run stable cargo clippy ...` before
  trusting a green local clippy if CI still fails.
