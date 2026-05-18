# chatty-core

UI-agnostic core of the Chatty agent. Contains every piece of logic that
both the GPUI desktop app and the Ratatui terminal app share: models,
services, tools, settings, repositories, factories, exporters, and the
sandbox.

**This crate has no GPUI dependency.** Optional GPUI `Global` trait impls
are gated behind the `gpui-globals` feature; `chatty-gpui` enables it,
`chatty-tui` does not.

## Module layout

| Path | Responsibility |
|---|---|
| `auth/` | Azure / OAuth credential acquisition |
| `curated_mcp.rs` | Built-in MCP catalog seeded on first launch |
| `exporters/` | Conversation export (markdown, PDF, ATIF, …) |
| `factories/agent_factory/` | Build `AgentClient` per provider; tool registration |
| `gpui_globals.rs` | `impl Global` for core types (feature-gated) |
| `install.rs` | First-launch installation tasks |
| `models/` | Pure data types: `Conversation`, `Message`, stores |
| `repositories/` | Persistence abstractions (SQLite for conversations) |
| `sandbox/` | Bollard/Docker sandbox backend + manager |
| `services/` | Math/Mermaid renderers, shell, MCP, sync, … |
| `settings/` | Settings models + JSON repositories |
| `token_budget/` | Tokenization, context-window accounting, summarizer |
| `tools/` | LLM tool implementations (filesystem, shell, MCP, …) |

## Process-global singletons

A single comment block at the top of [`src/lib.rs`](src/lib.rs) lists every
process-global singleton in the workspace, where it lives, and the
rationale for whether it's centralized or domain-local. Update that
inventory whenever you add or move a singleton.

## Build / test

From the workspace root:

```bash
cargo test -p chatty-core --lib      # fast inner loop (no integration tests)
cargo test -p chatty-core            # includes integration tests
make test                            # full CI-equivalent run (--test-threads=1)
```

See [`AGENTS.md`](../../AGENTS.md) for repo-wide conventions and
[`CLAUDE.md`](../../CLAUDE.md) for coding patterns.
