# chatty-gpui

Desktop frontend for Chatty, built on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui).
Ships the `chatty` binary.

This crate is the GPUI-specific layer on top of [`chatty-core`](../chatty-core/).
All UI-agnostic state (settings models, tools, services, repositories)
lives in `chatty-core` and is consumed here.

## Module layout

| Path | Responsibility |
|---|---|
| `main.rs` | Application entry point; Tokio runtime + GPUI app setup |
| `auto_updater/` | In-app update download / install (macOS .dmg, Linux AppImage, Windows .exe) |
| `assets.rs` | Embedded asset loading for the GPUI app |
| `cli_installer.rs` | Installs the `chatty-cli` companion symlink |
| `global_entity.rs` | Helper for storing `WeakEntity<T>` in globals |
| `chatty/controllers/` | Top-level app controller (`ChattyApp`), message ops, conversation ops, slash commands |
| `chatty/views/` | All GPUI views: chat, sidebar, chat input, trace components, … |
| `chatty/models/` | GPUI-specific entity wrappers (e.g. `StreamManager`) |
| `chatty/services/` | GPUI-specific services (theme, window, notifications) |
| `chatty/token_budget/` | GPUI token-budget UI manager |
| `settings/views/` | Settings window views (providers, models, MCP, …) |

`crates/chatty-gpui/src/chatty/mod.rs` also **re-exports** several modules
from `chatty-core` for ergonomic `use crate::chatty::…` paths:
`auth`, `exporters`, `factories`, `repositories`, `tools`. If grep doesn't
find a definition under this crate, look in `chatty-core/`.

## Patterns

Read [`CLAUDE.md`](../../CLAUDE.md) before changing UI code. Key rules:

- **Event-driven communication** between entities — `EventEmitter` +
  `cx.subscribe()`, never `Arc<dyn Fn>` callbacks.
- **Weak entity refs** in globals — never strong references.
- **Optimistic updates** — mutate the in-memory global immediately, then
  persist asynchronously with a logged error path.
- **Stream lifecycle** flows through `StreamManager` — the stream loop
  never updates the UI directly, it emits events.

## Run / build / test

```bash
make run-gpui                   # cargo run -p chatty-gpui
cargo test -p chatty-gpui       # unit + light integration
make test                       # full CI-equivalent run
```

On Linux you need a handful of system packages — run `make setup` once.

See [`AGENTS.md`](../../AGENTS.md) for the repo-wide cheat sheet.
