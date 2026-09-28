# chatty-module-sdk

SDK for authoring chatty WASM plugins against `chatty:plugin@0.3.0`.
**Compile target: `wasm32-wasip2`.**

A plugin contributes tools to a chatty agent and never runs a loop of its
own: the agent's model decides when to call a tool, and the host calls the
plugin's `invoke-tool` (PL-D1 option B).

This crate is **intentionally outside the main workspace** (see the
`[workspace]` table at the bottom of `Cargo.toml`). Plugin authors depend on
it as a `path = "../../crates/chatty-module-sdk"` from their own single-crate
project rooted at the plugin directory.

## What it provides

- **WIT types**, generated from `wit/chatty-plugin.wit`: `PluginMetadata`,
  `Capability`, `ToolDefinition`, `ToolCallRequest`, `ToolResult`,
  `ToolError`, `Message`, `Role`, …
- **Host imports**, one module per capability: `llm::complete`,
  `config::get`, `log::info`/`warn`/…, `file::read_bytes`, `billing::*`. A
  plugin lists the capabilities it uses in `metadata().requested_capabilities`;
  `logging` is always granted.
- **`Plugin`**, the trait a plugin implements (`metadata`, `list_tools`,
  `invoke_tool`), and **`export!`**, which wires it to the component's
  exports. Both are wit-bindgen's own generated code, so the export names
  always match the WIT.

`hive-billing-sdk` uses this crate's `billing` imports, so a paid plugin can
depend on both (the `billing` test fixture links them together).

## Quick start

See the rustdoc example at the top of [`src/lib.rs`](src/lib.rs), the
reference plugins under [`../../modules/`](../../modules/) (`echo`,
`benford`), and the
[Build a WASM plugin guide](../../docs-site/src/dev/guides/build-wasm-module.md).

## Build

```bash
rustup target add wasm32-wasip2
cd modules/your-plugin
cargo build --target wasm32-wasip2 --release
```

The output `.wasm` lives under
`target/wasm32-wasip2/release/your_plugin.wasm`. Copy it next to your
`module.toml` so the registry can discover it.

## WIT version

`WIT_PACKAGE` is `chatty:plugin@0.3.0`. The build fails if
`wit/chatty-plugin.wit` declares any other package: a WIT version bump has
to update `WIT_PACKAGE` here and in `chatty-wasm-runtime` deliberately, since
the host refuses every other world with "rebuild it with the current SDK".
