# echo

Reference chatty plugin (`chatty:plugin@0.4.0`), the canonical quickstart for
plugin authors.

**Tutorial:** [write a plugin](https://boersmamarcel.github.io/chatty2/dev/start/tutorial-echo-agent.html)
(mdBook) · full source in this directory.

This plugin is both:

* **Reference implementation**: a whole plugin in ~80 lines.
* **End-to-end integration test**: CI's whole-workspace `cargo test
  --all-features` run (after `scripts/build-wasm-fixtures.sh` builds this
  plugin) includes `chatty-protocol-gateway`'s `echo_plugin_e2e` suite
  against it.

A plugin contributes tools to an agent; it is never an agent itself and runs
no loop of its own. An agent spec lists it under `[[plugins]]`, and the
agent's model sees its tools as `echo__echo`, `echo__reverse` and
`echo__count_words`.

---

## What it does

| Export | Behaviour |
|--------|-----------|
| **metadata** | name `"echo"`, version `0.2.0`, requests no capability (`logging`, which it uses, is always granted) |
| **list-tools** | `echo`, `reverse`, `count_words`, each taking `{"input": string}` |
| **invoke-tool** | `echo` returns the input unchanged · `reverse` reverses its characters · `count_words` returns the word count; an unknown tool is an `unknown-tool` error, bad arguments `invalid-arguments` |

---

## Build in 10 minutes

### Prerequisites

```sh
# Rust toolchain (stable)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# WASM target (wasm32-wasip2)
rustup target add wasm32-wasip2
```

### Build

```sh
cd modules/echo
cargo build --target wasm32-wasip2 --release

# Copy the WASM next to module.toml so the registry can find it
cp target/wasm32-wasip2/release/echo.wasm .
```

### Verify

```sh
# Inspect the component's exports (requires wasm-tools): it exports
# chatty:plugin/plugin@0.4.0 and imports only what it uses.
wasm-tools component wit echo.wasm
```

---

## Project layout

```
modules/echo/
├── Cargo.toml          # cdylib, standalone [workspace]
├── .cargo/config.toml  # sets default target to wasm32-wasip2
├── module.toml         # registry manifest (name, version, wasm path, …)
├── src/
│   └── lib.rs          # the Plugin implementation + export!
└── README.md           # this file
```

---

## How it works

The SDK exposes three layers:

### 1. Types

```rust
use chatty_module_sdk::{
    export, Plugin, PluginMetadata, ToolCallRequest, ToolDefinition, ToolError, ToolResult,
};
```

Generated from `wit/chatty-plugin.wit`.

### 2. Host imports, one per capability

```rust
// Capability `llm`: a completion on the calling agent's model
let resp = chatty_module_sdk::llm::complete("", &messages, None)?;

// Capability `config`: a value from module.toml's [config]
let val = chatty_module_sdk::config::get("my-key");

// Capability `logging` (always granted): forwarded to tracing on the host
chatty_module_sdk::log::info("hello from wasm");
```

A plugin names the capabilities it uses in
`metadata().requested_capabilities`.

### 3. Trait + macro

```rust
pub struct MyPlugin;

impl Plugin for MyPlugin {
    fn metadata() -> PluginMetadata { ... }
    fn list_tools() -> Vec<ToolDefinition> { ... }
    fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> { ... }
}

export!(MyPlugin);   // wit-bindgen's generated export glue
```

---

## Building your own plugin

Use the cargo-generate template from the repository root:

```sh
cargo generate --path templates/module --name my-plugin
cd my-plugin
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/my_plugin.wasm .
```

Then copy the directory into your chatty modules folder and list it in an
agent spec (`[[plugins]] module = "my-plugin"`).

---

## Running the end-to-end tests

After `scripts/build-wasm-fixtures.sh`:

```sh
# From the workspace root
cargo test -p chatty-protocol-gateway --test echo_plugin_e2e
```

The steps:

1. The module registry discovers and loads echo
2. `list_tools()` returns the three tools
3. `invoke_tool(echo, {"input":"hello"})` → `"hello"`
4. `invoke_tool(reverse, {"input":"hello"})` → `"olleh"`
5. `metadata()` names the plugin and requests nothing
6. `GET /.well-known/agent.json` does not list it: a plugin is not an agent
7. `POST /mcp/echo` `tools/list` → three tools
8. `POST /mcp/echo` `tools/call count_words` → `"3"`
9. No OpenAI or A2A route answers for it

`crates/chatty-tui/tests/plugins_headless.rs` runs it inside an agent:
`chatty-tui --headless` with a spec listing `echo` calls `echo__reverse`.
