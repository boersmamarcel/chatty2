# Documentation index

A short pointer page so agents and humans can scan all of `docs/` without
listing the directory. Files are grouped by purpose.

For the top-level orientation read [`AGENTS.md`](../AGENTS.md) first;
for coding patterns and behavioural rules read
[`CLAUDE.md`](../CLAUDE.md).

## Architecture & design

| File | What it covers |
|---|---|
| [`architecture-overview.md`](architecture-overview.md) | Workspace structure, data flow, persistence, key design decisions |
| [`workspace-crate-split.md`](workspace-crate-split.md) | Rationale for the chatty-core / chatty-gpui / chatty-tui split |
| [`entity-communication.md`](entity-communication.md) | EventEmitter / `cx.subscribe()` pattern between GPUI entities |
| [`stream-manager.md`](stream-manager.md) | Centralized LLM stream lifecycle, cancellation, events |
| [`rendering-system.md`](rendering-system.md) | Markdown / math / mermaid rendering pipeline |
| [`token-tracking.md`](token-tracking.md) | Token budget accounting and summarization |
| [`agent-memory.md`](agent-memory.md) | Persistent agent memory store |

## Modules / extensions

| File | What it covers |
|---|---|
| [`a2a-and-wasm-modules.md`](a2a-and-wasm-modules.md) | End-to-end WASM agent module flow, A2A protocol |
| [`wit-reference.md`](wit-reference.md) | WIT interface schemas (`llm`, `config`, `logging`, `billing`) |
| [`curated-mcp-catalog.md`](curated-mcp-catalog.md) | Built-in MCP server catalog seeded on first launch |
| [`pre-built-apis.md`](pre-built-apis.md) | Pre-built API integrations bundled with the app |

## Operations

| File | What it covers |
|---|---|
| [`RELEASE_PROCESS.md`](RELEASE_PROCESS.md) | Cutting a release (version bump, changelog, GitHub Release) |
| [`monty-sandbox.md`](monty-sandbox.md) | Docker-backed sandbox runtime |
| [`debug_ui.md`](debug_ui.md) | Diagnosing chat-view rendering bugs (whitespace, overlap) — tracing + `CHATTY_DEBUG_UI` overlay |
| [`refactor-followups.md`](refactor-followups.md) | Open items from the agent-friendliness refactor (deferred Tier 4 splits + Tier 5 recommendations) |

---

**Adding a doc?** Append a row to the appropriate section above so this
index stays one-glance complete.
