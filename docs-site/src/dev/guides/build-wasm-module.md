# Build a WASM plugin

**When to read this:** you want to ship tools that a chatty agent loads as a sandboxed WASM plugin.

## Goal

A `wasm32-wasip2` component targeting `chatty:plugin@0.3.0`, built from the repo template, installed in the module directory, and listed in an agent spec so the agent's model can call its tools.

A plugin contributes **tools**; it never runs a loop and is never an agent. It implements the [`Plugin`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-module-sdk/src/lib.rs) trait: `metadata` (name, version, the host capabilities it requests, the config keys it reads), `list_tools` and `invoke_tool`. The host provides one import per capability: `llm::complete` (a completion on the calling agent's model; API keys stay on the host), `config::get` (the manifest's `[config]` table), `logging::log` (always granted), `file::read-bytes` (reads under a manifest-granted root) and `billing` (paid plugins). The contract is in the [WIT reference](../architecture/wit-reference.md).

## Prerequisites

- The `wasm32-wasip2` target: `make setup`, or `rustup target add wasm32-wasip2`.
- [`cargo-generate`](https://github.com/cargo-generate/cargo-generate) if you want to scaffold from the template (`cargo install cargo-generate`).

## Steps

### 1. Scaffold

```sh
# From a clone of the repo
cargo generate --path templates/module --name my-plugin

# Or straight from GitHub
# cargo generate --git https://github.com/boersmamarcel/chatty2 --name my-plugin templates/module
cd my-plugin
```

### 2. Know the project layout

```text
my-plugin/
├── Cargo.toml              # cdylib; standalone [workspace]
├── .cargo/config.toml      # default target = wasm32-wasip2
├── module.toml             # registry manifest
├── src/lib.rs              # impl Plugin + export!
└── my_plugin.wasm          # built artifact (next to module.toml)
```

The SDK is a path dependency when the plugin lives under `modules/` in the repo:

```toml
[dependencies]
chatty-module-sdk = { path = "../../crates/chatty-module-sdk" }
```

The template's `module.toml` declares `name`, `version`, `description` and `wasm` under `[module]`, `[capabilities] tools`, `[protocols] mcp` (serve the tools to external MCP clients at `/mcp/{name}`) and `[resources]` (`max_memory_mb`, `max_execution_ms`; a manifest may only lower the host ceilings). `[config]` and `[files] root` are optional. Parsing is strict: an unknown key is an error that names it (the 0.2.0 keys `chat` and `openai_compat` included).

### 3. Implement `Plugin`

Fill in `src/lib.rs`: `metadata`, `list_tools`, `invoke_tool`, and `export!(MyPlugin)`. A tool reads its arguments out of `call.arguments_json` (one JSON object, shaped by the schema it declared) and returns `ToolResult::text(...)` or a `ToolError` (`unknown_tool`, `invalid_arguments`, `denied`, `failed`); the model reads an error as `<kind>: <message>`.

A tool that needs the model requests `Capability::Llm` in its metadata and calls:

```rust
use chatty_module_sdk::{llm, Message, Role};

let messages = [Message::new(Role::User, question)];
let reply = llm::complete("", &messages, None).map_err(ToolError::failed)?; // "" = the calling agent's model
```

The call goes through the calling agent's provider client and its usage is counted as the plugin's own line in the turn. Worked examples: [Tutorial: write a plugin](../start/tutorial-echo-agent.md) (echo) and [Tutorial: give an agent the plugin](../start/tutorial-benford-agent.md) (benford).

### 4. Build and install

```sh
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/my_plugin.wasm .

# Linux example; macOS is ~/Library/Application Support/chatty/modules/, Windows %APPDATA%\chatty\modules\
mkdir -p ~/.local/share/chatty/modules/my-plugin
cp -r . ~/.local/share/chatty/modules/my-plugin/
```

### 5. Give an agent the plugin

List it in a spec, e.g. `.chatty/agents/my-agent.toml`:

```toml
[agent]
name = "my-agent"
preamble = "Use my-plugin's tools to …"

[[plugins]]
module = "my-plugin"
```

and run `chatty-tui --agent my-agent`. The model sees each tool as `my-plugin__<tool>`.

### 6. Test your plugin

Unit tests for pure-Rust logic run on the host target inside the plugin directory (pass `--target <your host triple>`: the template's `.cargo/config.toml` defaults to `wasm32-wasip2`, so a bare `cargo test` tries to execute the `.wasm`). For the load/export/MCP round trip and a real agent calling the plugin, the repo's echo suites are the model to copy:

```sh
# From the repo root — builds every plugin and fixture
make wasm-modules
cargo test -p chatty-protocol-gateway --test echo_plugin_e2e
cargo test -p chatty-tui --test plugins_headless
```

## Verify

- `chatty-tui --agent my-agent --headless -m "…"` calls `my-plugin__<tool>`.
- With the desktop's module gateway on, `POST http://localhost:8420/mcp/my-plugin` `tools/list` shows your tools (if `[protocols] mcp = true`).
- A plugin built against another WIT world is refused at load with `module targets …; this chatty supports chatty:plugin@0.3.0 — rebuild it with the current SDK`.

## Checklist

- [ ] `wasm32-wasip2` target installed
- [ ] `metadata().name`/`version` match `[module]` in `module.toml`
- [ ] Every host import the plugin calls is in `requested_capabilities` (`logging` needs no request)
- [ ] `.wasm` copied next to `module.toml`
- [ ] An agent spec lists the plugin under `[[plugins]]`
- [ ] Host-target unit tests for your tool logic

## Common mistakes

| Mistake | Fix |
|---------|-----|
| The agent has no `my-plugin__*` tools | The spec must list the plugin; the plugin must be in the module directory (Settings → Modules) |
| "rebuild it with the current SDK" at load | The plugin was built against an older world; rebuild it against this repo's SDK |
| Built for the host target | Use the `.cargo/config.toml` from the template, or pass `--target wasm32-wasip2` |
| `.wasm` name does not match `[module].wasm` | The manifest path is relative to `module.toml` |
| Calling `llm::complete` with a model id chatty does not have | Pass `""` for the calling agent's model |
| Expecting the plugin to be an agent (`list_agents`, `/agent`, A2A) | A plugin is tools only; the agent is the spec that loads it |
| A tool name over the limit | `<plugin>__<tool>` must be at most 64 characters of `[a-zA-Z0-9_-]` |

## Reference

| Topic | Doc |
|-------|-----|
| WIT types, host imports, the guest export, versioning | [WIT reference](../architecture/wit-reference.md) |
| Gateway routes, manifest fields, resource limits, module directory per platform | [Plugins](../architecture/plugins.md) |
| Crate stack diagram | [Component map](../architecture/component-map.md) |
| Template source | [`templates/module/`](https://github.com/boersmamarcel/chatty2/tree/main/templates/module) |
| Reference plugins | [`modules/echo/`](https://github.com/boersmamarcel/chatty2/tree/main/modules/echo), [`modules/benford/`](https://github.com/boersmamarcel/chatty2/tree/main/modules/benford) |
| SDK rustdoc | [`chatty-module-sdk`](https://github.com/boersmamarcel/chatty2/tree/main/crates/chatty-module-sdk) (`cargo doc -p chatty-module-sdk --open`) |
