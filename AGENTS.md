# AGENTS.md

<!-- DO NOT REMOVE: ownership contract pointer. Regenerating this file must preserve it. -->
> ## Research work: read [`RESERVED.md`](RESERVED.md) first
>
> The Linear project **Self-improving chatty2** lands five agentic-self-improvement papers
> in this workspace as `chatty-trace`, `chatty-playbook`, `chatty-flow`, and
> `chatty-optimize` (search + paired stats + optimizer QA loaders). **Stage B sandboxes**
> (HumanEval, Polyglot, AppWorld, …) live in the sibling repo `harbor-chatty` (Harbor;
> Linear AGE-34), not in this workspace. There is **no `chatty-eval` crate**. **A small
> set of functions in those crates is reserved for the human and must not be implemented
> by an agent** — the list and the rules are in [`RESERVED.md`](RESERVED.md), enforced by
> `scripts/check-reserved.sh` in CI.
>
> Take only `owner:ai` issues unless told otherwise. Never answer or close a
> `gate:reflection` issue. Ordinary chatty2 work is unaffected by any of this.
>
> ### Auto-ship (zero-human patch releases)
>
> Low-risk work may merge and **patch-release** without Marcel when — and only when —
> all of the following hold:
>
> 1. The Linear issue lives in project **[Chatty auto-ship](https://linear.app/agents-research/project/chatty-auto-ship-5f83bdaf5c5e)** (not Self-improving chatty2).
>    Project members = Marcel (closed-system allowlist).
> 2. The GitHub PR is on branch `auto/*`, titled `auto: …`, and carries labels
>    `ship:auto` + `release:patch` (never minor/major). Privileged labels are
>    owner / `github-actions[bot]` only; outsiders are stripped.
> 3. CI + `ship-auto-guard` are green; then auto-merge (same-repo head; owner sender
>    or Actions actor) → `prepare-release` lands the version bump via a `cut-release`
>    PR (main is protected), tags, and builds.
>
> **Project membership is the allowlist.** Do not auto-merge solely because an issue has
> `owner:ai` on another project. Never auto-ship reserved symbols, research crates,
> auth/billing, or core UX. Slack `#chatty-auto-ship` is notify-only. Failures notify
> Linear, Slack, and GitHub. Emergency rebuild (owner only; run against the tag):
> `gh workflow run release.yml --ref vX.Y.Z -f tag_name=vX.Y.Z`.
> Authz regression: `bash scripts/check-release-authz.sh`.

Quick-start guide for AI coding agents working in this repository.
Optimized for limited context windows: read this first, then dive deeper
via the links below.

For human-oriented documentation, see [`README.md`](README.md). For
detailed coding patterns and behavioral guidelines, see
[`CLAUDE.md`](CLAUDE.md). For full architecture details, see
[`docs/`](docs/).

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

```bash
make setup            # one-time: install Linux deps + wasm32-wasip2 target
make build            # cargo build (debug)
make test             # cargo test --all-features -- --test-threads=1  (matches CI)
make test-fast        # cargo test -p chatty-core --lib  (quick inner loop)
make test-tui         # cargo test -p chatty-tui          (TUI changes only)
make test-gpui        # cargo test -p chatty-gpui         (GPUI changes only)
make test-gateway     # cargo test -p chatty-protocol-gateway  (gateway changes only)
make lint             # cargo clippy -- -D warnings
make fmt              # cargo fmt
make fmt-check        # cargo fmt --check
make wasm-modules     # build the echo-agent WASM module (needed by tests)
make docs-gen         # regenerate docs/generated reference pages
make docs             # sync + build mdBook site (docs-site/book/)
make docs-serve       # local preview at http://localhost:3000
make docs-check-links # lychee link check (AGE-117)
make docs-check-nav   # INDEX.md + SUMMARY.md drift check (AGE-116)
make ci               # everything CI runs, locally, in order
```

Or use cargo directly:

```bash
cargo build
cargo test --all-features -- --test-threads=1
cargo fmt --check
cargo clippy -- -D warnings
cargo build -p chatty-tui
./target/debug/chatty-tui --help
```

### Test-thread footgun

CI runs tests with `--test-threads=1` because `chatty-core` tests
intermittently SIGTRAP under parallel execution on GitHub-hosted runners.
**If you see a SIGTRAP in CI but tests pass locally, run with
`--test-threads=1` locally to reproduce.** Root cause is unknown; the
workaround is documented in `.github/workflows/ci.yml`.

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
- **Weak entity refs in globals** — Always `WeakEntity<T>`, never strong
  refs (avoid circular ownership).
- **Stream lifecycle** — All LLM streams go through `StreamManager` with
  cancellation tokens; the stream loop never updates UI directly, it
  emits events. See [`docs/stream-manager.md`](docs/stream-manager.md).
- **Error handling** — Don't `.ok()` away errors silently. Log as
  `warn!()` for non-critical paths; propagate with `?` for critical I/O.
- **Sensitive env vars** — When sending MCP config to the LLM, use
  `masked_env()` not `.env`. See "Security Practices" in CLAUDE.md.
- **Rust edition** — 2024. Use `LazyLock`/`OnceLock` (std) rather than
  `lazy_static`/`once_cell`.

## Known gotchas

1. **`chatty-gpui::chatty::*` re-exports.** `crates/chatty-gpui/src/chatty/mod.rs`
   re-exports several modules (`auth`, `exporters`, `factories`,
   `repositories`, `tools`) from `chatty_core`. So
   `use crate::chatty::tools::Foo` in chatty-gpui actually resolves to
   `chatty_core::tools::Foo`. If grep finds no definition under
   `chatty-gpui/`, look in `chatty-core/`.

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

7. **Large module directories.** Several complex areas have been split
   into sub-module directories (`chat_view/`, `chat_input/`,
   `auto_updater/`, `trace_components/`, etc.). Start with the `mod.rs`
   and its module-level docstring to scope what you need before loading
   sibling files. The largest single files are `message_ops.rs` (~1260
   lines) and `main.rs` (~1225 lines).

## Deeper reading

| Topic | File |
|---|---|
| Full architecture | [`docs/architecture-overview.md`](docs/architecture-overview.md) |
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
| Release process | [`docs/RELEASE_PROCESS.md`](docs/RELEASE_PROCESS.md) |
| Coding patterns & behavior | [`CLAUDE.md`](CLAUDE.md) |

## Cursor Cloud specific instructions

Build/test/lint/run commands are unchanged — use the ones documented above
(the `make` targets / `.github/workflows/ci.yml`). The notes below are only
the non-obvious environment caveats specific to the Cloud Agent VM. System
packages, the Rust toolchain, and the `cc`/`c++` alternatives below are baked
into the VM; the startup/update script only re-runs
`rustup target add wasm32-wasip2` + `make wasm-modules`.

- **Toolchain must be ≥ 1.85 (edition 2024).** The base VM image historically
  pinned `rustup default` to an older toolchain (1.83), which cannot compile
  this workspace (`feature edition2024 is required`). The default is set to
  `stable`. If a build suddenly fails with an edition-2024 error, run
  `rustup default stable`.

- **Use GNU `cc`/`c++`, not clang.** `/usr/bin/cc` and `/usr/bin/c++` are
  pointed at `gcc`/`g++` via `update-alternatives`. The system clang cannot
  find libstdc++ headers/lib, which breaks the bundled DuckDB C++ build
  (`fatal error: 'memory' file not found`) and linking the desktop binary
  (`rust-lld: error: unable to find library -lstdc++`). If you see either
  error, run `sudo update-alternatives --set cc /usr/bin/gcc` and
  `sudo update-alternatives --set c++ /usr/bin/g++`.

- **`modules/echo-agent/echo_agent.wasm` is git-ignored** and required by some
  integration tests, so it must be regenerated after a fresh checkout. The
  update script runs `make wasm-modules`; run it yourself if `make test` fails
  with a missing-`.wasm` error.

- **Running the GPUI desktop app (`chatty`) headlessly.** The VM has no GPU,
  but software Vulkan (Mesa `llvmpipe`/lavapipe) is installed, so the app runs
  with CPU rendering on the existing X11 display (`DISPLAY=:1`). You MUST export
  `XDG_RUNTIME_DIR` first, e.g.
  `export XDG_RUNTIME_DIR=/tmp/xdg-runtime-$(id -u) && mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"`,
  otherwise X11 window creation fails with `BadMatch`. `chatty-tui` needs none
  of this.

- **No LLM API key is configured.** To actually chat (either frontend), use a
  local Ollama: start it with `ollama serve` (systemd is not running, so launch
  it yourself in a background/tmux session), `ollama pull qwen2.5:0.5b`, then
  `./target/debug/chatty-tui --ollama http://localhost:11434 --model qwen2.5:0.5b --headless -m "..."`.
  The desktop app auto-detects a running local Ollama and lists its models.

- **Clippy has pre-existing findings under current stable.** `cargo clippy -- -D warnings`
  currently fails on 3 lints that predate this environment work
  (`models/conversations_store.rs`, `services/skill_service.rs`,
  `tools/search_memory_tool.rs`) — newer-toolchain lints, not env breakage.
