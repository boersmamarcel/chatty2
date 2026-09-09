# Workspace crate split

**When to read this:** You are about to add code and need to decide which crate it
belongs in, or you need to know how chatty-core types become GPUI globals.

Chatty is a Cargo workspace. The three application crates:

| Crate | Purpose | GPUI dependency? |
|:------|:--------|:-----------------|
| **chatty-core** | Models, services, tools, settings, repositories, factories | No |
| **chatty-gpui** | Desktop UI: views, controllers, stream manager, notifiers | Yes |
| **chatty-tui** | Terminal UI: single-session chat, headless/pipe mode | No |

chatty-core never depends on either frontend. The dependency direction is:

```
chatty-gpui ──depends on──► chatty-core (with the gpui-globals feature)
chatty-tui  ──depends on──► chatty-core (without it)
```

The other workspace crates (WASM runtime, module registry, protocol gateway, module
SDK, Hive clients, research crates) are listed in [system-overview.md](system-overview.md).

## The `gpui-globals` feature

chatty-core types (stores, settings models, services) must implement GPUI's `Global`
trait for chatty-gpui to use `cx.set_global()` / `cx.global()`, but chatty-core must
not link GPUI for the terminal build. The `impl Global` blocks therefore live in one
conditional module:

```toml
# crates/chatty-core/Cargo.toml
[features]
gpui-globals = ["dep:gpui"]

[dependencies]
gpui = { workspace = true, optional = true }
```

```rust
// crates/chatty-core/src/lib.rs
#[cfg(feature = "gpui-globals")]
mod gpui_globals;

// crates/chatty-core/src/gpui_globals.rs
impl Global for crate::settings::models::GeneralSettingsModel {}
impl Global for crate::models::ConversationsStore {}
// … every type that needs cx.set_global()
```

chatty-gpui enables the feature in its `chatty-core` dependency; chatty-tui does not,
and instead holds core services and models as fields on its `ChatEngine`.

To add a new global type: define it in chatty-core, add
`impl Global for crate::…::MyStore {}` to `gpui_globals.rs`, then call
`cx.set_global(MyStore::new())` from chatty-gpui as usual.

## Decision criteria

Ask: **"Does this code need GPUI's `Context`, `Window`, `Entity`, `Render`, or
`EventEmitter`?"**

- **Yes** → chatty-gpui
- **No, but it is terminal rendering or TUI-only orchestration** → chatty-tui
- **No** → chatty-core

Anything that both frontends could run — stream processing, tool execution, settings
persistence, token counting — belongs in chatty-core even if only one frontend uses
it today. The `ui-sync-check` workflow flags a change in one UI crate without the other.

## No re-exports of core modules

chatty-gpui and chatty-tui import chatty-core's UI-agnostic modules (`auth`,
`exporters`, `factories`, `repositories`, `tools`) as `chatty_core::…` at every call
site rather than re-exporting them under a local path, so a definition's crate is
visible from its import;
[`scripts/check-no-core-reexports.sh`](https://github.com/boersmamarcel/chatty2/blob/main/scripts/check-no-core-reexports.sh),
run in CI, fails the build if either crate re-introduces such a re-export.

> [!NOTE]
> If you grep for a definition under `crates/chatty-gpui/src/` and find nothing, it
> lives under `crates/chatty-core/src/`.
