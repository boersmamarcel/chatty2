# Documentation index

A short pointer page so agents and humans can scan all of `docs/` without
listing the directory. Files are grouped by purpose.

For the top-level orientation read [`AGENTS.md`](../AGENTS.md) first;
for coding patterns and behavioural rules read
[`CLAUDE.md`](../CLAUDE.md).

**Published site:** GitHub Pages mdBook (run `make docs-serve` locally).

## Architecture & design

| File | When to read | What it covers |
|---|---|---|
| [`system-overview.md`](system-overview.md) | First time in the repo | One-page mental model, message path diagram |
| [`component-map.md`](component-map.md) | Need diagrams of how parts connect | Crate/module/entity relationship visuals |
| [`architecture-overview.md`](architecture-overview.md) | Contributor onboarding | Workspace structure, data flow, persistence |
| [`workspace-crate-split.md`](workspace-crate-split.md) | Why core/gpui/tui exist | Crate split rationale |
| [`entity-communication.md`](entity-communication.md) | GPUI event wiring | EventEmitter / `cx.subscribe()` pattern |
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
| [`monty-sandbox.md`](monty-sandbox.md) | Code execution | Docker / Monty sandbox |
| [`debug_ui.md`](debug_ui.md) | Layout/rendering bugs | `CHATTY_DEBUG_UI` overlay |
| [`refactor-followups.md`](refactor-followups.md) | Large-file splits | Deferred agent-friendliness work |

## Research / ADRs

| File | When to read | What it covers |
|---|---|---|
| [`research/harbor-pivot.md`](research/harbor-pivot.md) | Stage B sandboxes | Harbor pivot decision |
| [`research/crate-promises-chatty-trace.md`](research/crate-promises-chatty-trace.md) | chatty-trace work | Trace crate scope |
| [`research/crate-promises-chatty-playbook.md`](research/crate-promises-chatty-playbook.md) | chatty-playbook work | Playbook crate scope |
| [`research/crate-promises-chatty-flow.md`](research/crate-promises-chatty-flow.md) | chatty-flow work | Flow crate scope |
| [`research/cost-model.md`](research/cost-model.md) | Optimizer economics | Cost model |
| [`research/appworld-decision.md`](research/appworld-decision.md) | Eval sandbox choice | AppWorld decision |

## Generated reference (`docs/generated/`)

Regenerate with `make docs-gen`. Synced into the mdBook site on build.

| File | When to read |
|---|---|
| `tools-catalog.md` | Look up LLM tool names |
| `slash-commands.md` | `/` commands in GPUI |
| `cli-flags.md` | `chatty-tui --help` |
| `env-vars.md` | `CHATTY_*` and related env vars |
| `llms.txt` | Agent discovery index |

---

**Adding a doc?** Append a row to the appropriate section above so this
index stays one-glance complete.
