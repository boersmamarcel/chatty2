# Documentation index

A short pointer page so agents and humans can scan all of `docs/` without
listing the directory. Files are grouped by purpose.

For the top-level orientation read [`AGENTS.md`](../AGENTS.md) first;
for coding patterns and behavioural rules read
[`CLAUDE.md`](../CLAUDE.md).

**Published site:** GitHub Pages mdBook (run `make docs-serve` locally).

## User guides (mdBook `/user/`)

End-user how-to pages are hand-written in `docs-site/src/user/` (not copies of
`docs/`). The repo [README.md](../README.md) is a short landing page that
points here.

| File | When to read |
|---|---|
| [`../docs-site/src/user/getting-started.md`](../docs-site/src/user/getting-started.md) | First-run download, provider, model, tools |
| [`../docs-site/src/user/overview.md`](../docs-site/src/user/overview.md) | What Chatty is (docs voice) |
| [`../docs-site/src/user/agents.md`](../docs-site/src/user/agents.md) | Agent loop, plans, context window |
| [`../docs-site/src/user/providers-and-models.md`](../docs-site/src/user/providers-and-models.md) | Providers, models, capabilities |
| [`../docs-site/src/user/agentic-tools.md`](../docs-site/src/user/agentic-tools.md) | Built-in tools and MCP |
| [`../docs-site/src/user/memory-and-skills.md`](../docs-site/src/user/memory-and-skills.md) | Persistent memory and skills |
| [`../docs-site/src/user/sub-agents.md`](../docs-site/src/user/sub-agents.md) | Headless `chatty-tui` children |
| [`../docs-site/src/user/security.md`](../docs-site/src/user/security.md) | Workspace, shell, approval, secrets |
| [`../docs-site/src/user/features.md`](../docs-site/src/user/features.md) | Rendering, traces, export, themes |
| [`../docs-site/src/user/terminal.md`](../docs-site/src/user/terminal.md) | `chatty-tui` modes and keybindings |
| [`user/README-SPLIT-TODO.md`](user/README-SPLIT-TODO.md) | AGE-97 split decisions (GIF placement) |

## Architecture & design

| File | When to read | What it covers |
|---|---|---|
| [`system-overview.md`](system-overview.md) | First time in the repo | One-page mental model, message path diagram |
| [`component-map.md`](component-map.md) | Need diagrams of how parts connect | Crate/module/entity relationship visuals |
| [`architecture-overview.md`](architecture-overview.md) | Contributor onboarding | Workspace structure, data flow, persistence |
| [`workspace-crate-split.md`](workspace-crate-split.md) | Why core/gpui/tui exist | Crate split rationale |
| [`entity-communication.md`](entity-communication.md) | GPUI event wiring | EventEmitter / `cx.subscribe()` pattern. How-to: [add a desktop GPUI view](https://github.com/boersmamarcel/chatty2/blob/main/docs-site/src/dev/guides/add-gpui-view.md) |
| [`stream-manager.md`](stream-manager.md) | Stream bugs or cancellation | LLM stream lifecycle, events |
| [`rendering-system.md`](rendering-system.md) | Markdown/math/mermaid UI | Rendering pipeline |
| [`token-tracking.md`](token-tracking.md) | Context window / compact | Token budget accounting |
| [`agent-memory.md`](agent-memory.md) | Memory tools / skills | Persistent agent memory store |

## Modules / extensions

| File | When to read | What it covers |
|---|---|---|
| [`a2a-and-wasm-modules.md`](a2a-and-wasm-modules.md) | WASM agents or A2A | End-to-end module flow |
| [`wit-reference.md`](wit-reference.md) | Authoring WASM modules | WIT interface schemas |
| [`curated-mcp-catalog.md`](curated-mcp-catalog.md) | Built-in MCP servers | Seeded MCP catalog |
| [`pre-built-apis.md`](pre-built-apis.md) | Bundled integrations | Pre-built API list |

## Operations

| File | When to read | What it covers |
|---|---|---|
| [`RELEASE_PROCESS.md`](RELEASE_PROCESS.md) | Cutting a release | Version bump, changelog, GH Release |
| [`stale-doc-policy.md`](stale-doc-policy.md) | Code changed, unsure if docs must | Same-PR rule, drift reports, update-agent-docs |
| [`monty-sandbox.md`](monty-sandbox.md) | Code execution | Docker / Monty sandbox |
| [`debug_ui.md`](debug_ui.md) | Layout/rendering bugs | `CHATTY_DEBUG_UI` overlay |
| [`refactor-followups.md`](refactor-followups.md) | Large-file splits | Deferred agent-friendliness work |

## Research / ADRs

| File | When to read | What it covers |
|---|---|---|
| [`research/app-research-bridge.md`](research/app-research-bridge.md) | **App ↔ research map** | Memory, context window, loop, traces → M0–M4 |
| [`research/paper-to-product-pipeline.md`](research/paper-to-product-pipeline.md) | Research pipeline | SOTA → experiment → product flow |
| [`research/modules/index.md`](research/modules/index.md) | Per-paper module work | M0–M4 overview and status |
| [`research/modules/m0-trace.md`](research/modules/m0-trace.md) | chatty-trace / M0 work | Trace contract module notes |
| [`research/modules/m1-react.md`](research/modules/m1-react.md) | ReAct substrate work | M1 strategy variants |
| [`research/modules/m2-aflow.md`](research/modules/m2-aflow.md) | chatty-flow / M2 work | AFlow workflow search |
| [`research/modules/m3-gepa.md`](research/modules/m3-gepa.md) | chatty-optimize / M3 work | GEPA prompt evolution |
| [`research/modules/m4-ace.md`](research/modules/m4-ace.md) | chatty-playbook / M4 work | ACE playbook deltas |
| [`research/promotion-log.md`](research/promotion-log.md) | After experiments | Marcel-only promotion verdicts |
| [`research/settings-integration-map.md`](research/settings-integration-map.md) | Product integration | Settings ↔ research mechanisms |
| [`research/experiment-protocol.md`](research/experiment-protocol.md) | Running evals | Stage A/B checklist, cost accounting |
| [`research/harbor-pivot.md`](research/harbor-pivot.md) | Stage B sandboxes | Harbor pivot decision |
| [`research/crate-promises-chatty-trace.md`](research/crate-promises-chatty-trace.md) | chatty-trace work | Trace crate scope |
| [`research/crate-promises-chatty-playbook.md`](research/crate-promises-chatty-playbook.md) | chatty-playbook work | Playbook crate scope |
| [`research/crate-promises-chatty-flow.md`](research/crate-promises-chatty-flow.md) | chatty-flow work | Flow crate scope |
| [`research/cost-model.md`](research/cost-model.md) | Optimizer economics | Cost model |
| [`research/appworld-decision.md`](research/appworld-decision.md) | Eval sandbox choice | AppWorld decision |
| [`research/README.md`](research/README.md) | Research folder entry | How the research docs are organized |
| [`user/README-SPLIT-TODO.md`](user/README-SPLIT-TODO.md) | User-guide split leftover | Temporary note from the Epic 2 migration |

## Workspace crates

One-line map of all 13 workspace crates. Full index with dependency diagram:
[crates.md](https://github.com/boersmamarcel/chatty2/blob/main/docs-site/src/dev/crates.md)
(mdBook: **Crates → Workspace crate index**).

| Crate | Purpose |
|---|---|
| `chatty-core` | UI-agnostic agent core: models, services, tools, settings, sandbox |
| `chatty-gpui` | GPUI desktop app (`chatty` binary) |
| `chatty-tui` | Ratatui terminal app (interactive, headless, pipe) |
| `chatty-wasm-runtime` | Wasmtime embedding and host WIT interfaces |
| `chatty-module-registry` | WASM module discovery, manifest, lifecycle |
| `chatty-protocol-gateway` | HTTP gateway: OpenAI / MCP / A2A |
| `chatty-module-sdk` | SDK for `wasm32-wasip2` agent modules |
| `chatty-trace` | Research: trace capture, ATIF export, feedback (M0) |
| `chatty-playbook` | Research: ACE playbook memory (M4) |
| `chatty-flow` | Research: AFlow workflow IR (M2) |
| `chatty-optimize` | Research: GEPA/AFlow optimizers, paired stats (M3) |
| `hive-client` | Hive module registry client |
| `hive-billing-sdk` | Hive billing SDK for WASM publishers |

## Crate READMEs

| File | When to read | What it covers |
|---|---|---|
| [`../crates/chatty-core/README.md`](../crates/chatty-core/README.md) | Core crate work | UI-agnostic models, tools, services |
| [`../crates/chatty-gpui/README.md`](../crates/chatty-gpui/README.md) | Desktop UI work | GPUI binary |
| [`../crates/chatty-tui/README.md`](../crates/chatty-tui/README.md) | Terminal UI work | Ratatui binary, headless mode |
| [`../crates/chatty-trace/README.md`](../crates/chatty-trace/README.md) | chatty-trace work | Trace capture crate |
| [`../crates/chatty-playbook/README.md`](../crates/chatty-playbook/README.md) | chatty-playbook work | ACE playbook crate |
| [`../crates/chatty-flow/README.md`](../crates/chatty-flow/README.md) | chatty-flow work | Workflow IR crate |
| [`../crates/chatty-optimize/README.md`](../crates/chatty-optimize/README.md) | Optimizer tooling | Offline GEPA/AFlow (not in app binary) |
| [`../crates/chatty-wasm-runtime/README.md`](../crates/chatty-wasm-runtime/README.md) | WASM runtime | Wasmtime agent modules |
| [`../crates/chatty-module-registry/README.md`](../crates/chatty-module-registry/README.md) | Module registry | Discovery and lifecycle |
| [`../crates/chatty-protocol-gateway/README.md`](../crates/chatty-protocol-gateway/README.md) | HTTP gateway | OpenAI / MCP / A2A protocols |
| [`../crates/chatty-module-sdk/README.md`](../crates/chatty-module-sdk/README.md) | WASM module SDK | Authoring agent modules |

## Generated reference (`docs/generated/`)

Regenerate with `make docs-gen`. Synced into the mdBook site on build.

| File | When to read |
|---|---|
| `tools-catalog.md` | Look up LLM tool names |
| `provider-matrix.md` | Provider auth, capabilities, TUI flags |
| `slash-commands.md` | `/` commands in GPUI |
| `cli-flags.md` | `chatty-tui --help` when the binary is already built; otherwise a static fallback |
| `env-vars.md` | `CHATTY_*` and related env vars |
| `settings-schema.md` | Persisted settings JSON files, models, defaults (AGE-101 pair review) |
| `event-catalog.md` | GPUI entity events and subscribers |
| `singleton-inventory.md` | Process-global state and repositories |
| `llms.txt` | Agent discovery index (curated links) |
| `llms-full.txt` | Concatenated key pages for large-context agents |

---

**Adding a doc?** Append a row to the appropriate section above so this
index stays one-glance complete.
