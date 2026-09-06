# Documentation index

A one-glance map of every documentation source in the repository, for agents
and humans who want to scan `docs/` without listing the directory. The
published site is the mdBook at https://boersmamarcel.github.io/chatty2/
(`make docs-serve` locally); its navigation is `docs-site/src/SUMMARY.md`.

Read [`AGENTS.md`](../AGENTS.md) first for the workspace map and build
commands. Coding rules for humans are on the site's Contributing patterns
page; the agent-facing version is [`CLAUDE.md`](../CLAUDE.md).

## User guide (hand-written, `docs-site/src/user/`)

| Page | When to read |
|---|---|
| [`getting-started.md`](../docs-site/src/user/getting-started.md) | First run: download, provider, model, first message |
| [`providers-and-models.md`](../docs-site/src/user/providers-and-models.md) | Connect OpenRouter, Ollama or Azure; manage the model roster |
| [`chatting.md`](../docs-site/src/user/chatting.md) | Rendering, artifacts, PR status bar, cost, themes |
| [`agents-and-tools.md`](../docs-site/src/user/agents-and-tools.md) | The agent loop and what the agent can do |
| [`extensions.md`](../docs-site/src/user/extensions.md) | Hive marketplace, built-in integrations, custom MCP servers |
| [`memory-and-skills.md`](../docs-site/src/user/memory-and-skills.md) | Persistent memory and saved skills |
| [`sub-agents.md`](../docs-site/src/user/sub-agents.md) | Headless child agents |
| [`security.md`](../docs-site/src/user/security.md) | Approval modes, sandboxing, secrets |
| [`terminal.md`](../docs-site/src/user/terminal.md) | `chatty-tui` install, modes, keybindings |
| [`advanced.md`](../docs-site/src/user/advanced.md) | Training-data export, where Chatty stores data, updates |

## Developer guide (hand-written, `docs-site/src/dev/`)

| Page | When to read |
|---|---|
| [`start/build-and-run.md`](../docs-site/src/dev/start/build-and-run.md) | Clone to running binary in 10 minutes |
| [`start/first-change.md`](../docs-site/src/dev/start/first-change.md) | Add an LLM tool end to end |
| [`start/tutorial-echo-agent.md`](../docs-site/src/dev/start/tutorial-echo-agent.md) | First WASM module tutorial |
| [`start/tutorial-benford-agent.md`](../docs-site/src/dev/start/tutorial-benford-agent.md) | Agentic WASM module tutorial |
| [`where-to-look.md`](../docs-site/src/dev/where-to-look.md) | Task → file/doc routing (How-to landing page) |
| [`guides/add-provider.md`](../docs-site/src/dev/guides/add-provider.md) | Add an LLM provider |
| [`guides/add-slash-command.md`](../docs-site/src/dev/guides/add-slash-command.md) | Add a `/` command to both front ends |
| [`guides/add-gpui-view.md`](../docs-site/src/dev/guides/add-gpui-view.md) | Add a desktop view or dialog |
| [`guides/build-wasm-module.md`](../docs-site/src/dev/guides/build-wasm-module.md) | Author a WASM plugin |
| [`guides/test.md`](../docs-site/src/dev/guides/test.md) | How the test suite is organised and run |
| [`guides/debug.md`](../docs-site/src/dev/guides/debug.md) | Debug overlay, logs, stream and rendering bugs |
| [`guides/build-package.md`](../docs-site/src/dev/guides/build-package.md) | Build and package for each platform |
| [`guides/contribute-docs.md`](../docs-site/src/dev/guides/contribute-docs.md) | Edit the docs, page template, checks |
| [`contributing-patterns.md`](../docs-site/src/dev/contributing-patterns.md) | The rules a PR is reviewed against |
| [`crates.md`](../docs-site/src/dev/crates.md) | Workspace crate index (Reference landing page) |
| [`ci-reference.md`](../docs-site/src/dev/ci-reference.md) | Make targets and CI workflows |
| [`doc-frontmatter.md`](../docs-site/src/dev/doc-frontmatter.md) | Optional YAML frontmatter schema |
| [`glossary.md`](../docs-site/src/dev/glossary.md) | Terms used across the docs |

## Architecture & explanation (`docs/`, synced to the site)

| File | When to read | What it covers |
|---|---|---|
| [`system-overview.md`](system-overview.md) | First time in the repo | Layers, crate roles, message path, startup, key design decisions |
| [`component-map.md`](component-map.md) | Need diagrams of how parts connect | Crate/module/entity relationship visuals |
| [`workspace-crate-split.md`](workspace-crate-split.md) | Deciding where code goes | Crate boundaries, the `gpui-globals` feature |
| [`entity-communication.md`](entity-communication.md) | GPUI event wiring | `EventEmitter` / `cx.subscribe()` pattern |
| [`stream-manager.md`](stream-manager.md) | Stream bugs or cancellation | LLM stream lifecycle, events |
| [`rendering-system.md`](rendering-system.md) | Markdown/math/mermaid UI | Rendering pipeline and caches |
| [`token-tracking.md`](token-tracking.md) | Context window, cost | Token budget accounting |
| [`context-compaction.md`](context-compaction.md) | Long conversations, `/compact` | How compaction works |
| [`agent-memory.md`](agent-memory.md) | Memory tools / skills | Persistent agent memory store |
| [`a2a-and-wasm-modules.md`](a2a-and-wasm-modules.md) | WASM agents or A2A | Module flow, manifest, limits |
| [`wit-reference.md`](wit-reference.md) | Authoring WASM modules | WIT interface schemas (reference) |
| [`curated-mcp-catalog.md`](curated-mcp-catalog.md) | Built-in MCP servers | Seeded catalog and community servers (reference) |
| [`RELEASE_PROCESS.md`](RELEASE_PROCESS.md) | Cutting a release | Labels, version bump, changelog, GitHub Release |
| [`build-disk-usage.md`](build-disk-usage.md) | `target/` eating the disk | Where build space goes, pruning |

## Research notes (`docs/research/`, synced to the site under Explanation)

| File | When to read | What it covers |
|---|---|---|
| [`research/README.md`](research/README.md) | Entry point | What M0–M4 are and where the ADRs live |
| [`research/app-research-bridge.md`](research/app-research-bridge.md) | App ↔ research map | Memory, context window, loop, traces → M0–M4 |
| [`research/paper-to-product-pipeline.md`](research/paper-to-product-pipeline.md) | Research pipeline | Paper → experiment → product flow |
| [`research/experiment-protocol.md`](research/experiment-protocol.md) | Running evals | Stage A/B checklist, cost accounting |
| [`research/settings-integration-map.md`](research/settings-integration-map.md) | Product integration | Settings ↔ research mechanisms |
| [`research/harbor-pivot.md`](research/harbor-pivot.md) | Stage B sandboxes | Harbor pivot decision |
| [`research/cost-model.md`](research/cost-model.md) | Optimizer economics | Cost model |
| [`research/appworld-decision.md`](research/appworld-decision.md) | Eval sandbox choice | AppWorld decision |
| [`research/modules/index.md`](research/modules/index.md) | Per-paper module work | M0–M4 overview and status |
| [`research/modules/m0-trace.md`](research/modules/m0-trace.md) | chatty-trace | Trace contract |
| [`research/modules/m1-react.md`](research/modules/m1-react.md) | ReAct substrate | M1 strategy variants |
| [`research/modules/m2-aflow.md`](research/modules/m2-aflow.md) | chatty-flow | AFlow workflow search |
| [`research/modules/m3-gepa.md`](research/modules/m3-gepa.md) | chatty-optimize | GEPA prompt evolution |
| [`research/modules/m4-ace.md`](research/modules/m4-ace.md) | chatty-playbook | ACE playbook deltas |

## Crate READMEs (`crates/*/README.md`, synced to the site under Reference)

| Crate | Purpose |
|---|---|
| [`chatty-core`](../crates/chatty-core/README.md) | UI-agnostic agent core: models, services, tools, settings, sandbox |
| [`chatty-gpui`](../crates/chatty-gpui/README.md) | GPUI desktop app (`chatty` binary) |
| [`chatty-tui`](../crates/chatty-tui/README.md) | Ratatui terminal app (interactive, headless, pipe) |
| [`chatty-wasm-runtime`](../crates/chatty-wasm-runtime/README.md) | Wasmtime embedding and host WIT interfaces |
| [`chatty-module-registry`](../crates/chatty-module-registry/README.md) | WASM module discovery, manifest, lifecycle |
| [`chatty-protocol-gateway`](../crates/chatty-protocol-gateway/README.md) | HTTP gateway: OpenAI / MCP / A2A |
| [`chatty-module-sdk`](../crates/chatty-module-sdk/README.md) | SDK for `wasm32-wasip2` agent modules |
| [`chatty-trace`](../crates/chatty-trace/README.md) | Research: trace capture, ATIF export, feedback (M0) |
| [`chatty-playbook`](../crates/chatty-playbook/README.md) | Research: ACE playbook memory (M4) |
| [`chatty-flow`](../crates/chatty-flow/README.md) | Research: AFlow workflow IR (M2) |
| [`chatty-optimize`](../crates/chatty-optimize/README.md) | Research: GEPA/AFlow optimizers, paired stats (M3) |
| [`hive-client`](../crates/hive-client/README.md) | Hive module registry client |
| [`hive-billing-sdk`](../crates/hive-billing-sdk/README.md) | Hive billing SDK for WASM publishers |

## Generated reference (`docs/generated/`, gitignored)

Regenerate with `make docs-gen`; the tables live in
`scripts/gen-docs-reference.sh` and CI diffs them against the source
(`make docs-check-reference`).

| File | When to read |
|---|---|
| `tools-catalog.md` | Look up an LLM tool name and its source module |
| `provider-matrix.md` | Provider auth, capabilities, TUI flags |
| `slash-commands.md` | `/` commands in GPUI and TUI |
| `cli-flags.md` | `chatty-tui --help` (live when the binary is built) |
| `env-vars.md` | `CHATTY_*` and related env vars |
| `settings-schema.md` | Persisted settings JSON: paths, fields, defaults |
| `event-catalog.md` | GPUI entity events and subscribers |
| `singleton-inventory.md` | Process-global state and repositories |
| `llms.txt`, `llms-full.txt` | Agent discovery index and concatenated key pages |

## Archived (`docs/archive/`, not synced)

Point-in-time plans and audits kept for their reasoning; open items live in
Linear. See [`archive/README.md`](archive/README.md).

---

**Adding a doc?** A new file under `docs/` needs a row above and an entry in
`docs-site/src/SUMMARY.md`; CI checks both. Working notes go to
`docs/archive/` instead.
