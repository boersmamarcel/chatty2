# Build a WASM plugin

**When to read this:** You want to ship a local agent module that Chatty loads as a WASM plugin and exposes through the protocol gateway (OpenAI / MCP / A2A).

## Goal

A `wasm32-wasip2` component, built from the repo template, installed in the modules directory, answering on the gateway and callable from a conversation with `/agent <name> …`.

A module is a sandboxed guest implementing the [`ModuleExports`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-module-sdk/src/lib.rs) trait: `chat`, `invoke_tool`, `list_tools`, `get_agent_card`. The host provides exactly three imports — `llm::complete` (the host-managed LLM; API keys stay on the host), `config::get` (per-module key/value config from the manifest) and `logging::log`. Everything else — tools, business logic, multi-turn loops — runs inside your guest. The contract is in the [WIT reference](../architecture/wit-reference.md); how a call travels from `invoke_agent` through the gateway and the runtime to your `chat` export (your module is never linked into `chatty-core`) is in [A2A and WASM modules](../architecture/a2a-and-wasm-modules.md).

## Prerequisites

- The `wasm32-wasip2` target: `make setup`, or `rustup target add wasm32-wasip2`.
- [`cargo-generate`](https://github.com/cargo-generate/cargo-generate) if you want to scaffold from the template (`cargo install cargo-generate`).
- Chatty running with **Settings → Modules** enabled; the gateway then serves modules on `http://localhost:8420` (the default port).

## Steps

### 1. Scaffold

```sh
# From a clone of the repo
cargo generate --path templates/module --name my-agent

# Or straight from GitHub
# cargo generate --git https://github.com/boersmamarcel/chatty2 --name my-agent templates/module
cd my-agent
```

### 2. Know the project layout

```text
my-agent/
├── Cargo.toml              # cdylib; standalone [workspace]
├── .cargo/config.toml      # default target = wasm32-wasip2
├── module.toml             # registry manifest
├── src/lib.rs              # impl ModuleExports + export_module!
└── my_agent.wasm           # built artifact (next to module.toml)
```

The SDK is a path dependency when the module lives under `modules/` in the repo:

```toml
[dependencies]
chatty-module-sdk = { path = "../../crates/chatty-module-sdk" }
```

The template's `module.toml` declares `name`, `version`, `description` and `wasm` under `[module]`, `[capabilities]` (`tools`, `chat`, `agent`), `[protocols]` (`openai_compat`, `mcp`, `a2a`) and `[resources]` (`max_memory_mb`, `max_execution_ms`). Keep `[protocols].a2a = true`: it is what makes the module appear in `list_agents` and invocable with `invoke_agent`. Field-by-field meaning and the resource defaults are in the manifest section of [A2A and WASM modules](../architecture/a2a-and-wasm-modules.md).

### 3. Implement `ModuleExports`

Fill in `src/lib.rs`. To call the host LLM, pass an empty model string for the host default (or a model id configured in Chatty), your message history, and optionally a JSON array of tool definitions:

```rust
use chatty_module_sdk::{llm, Message, Role};

let messages = vec![
    Message { role: Role::System, content: "You are helpful.".into() },
    Message { role: Role::User, content: user_prompt.into() },
];

let resp = llm::complete("", &messages, None)?;          // plain completion
let resp = llm::complete("", &messages, Some(TOOLS_JSON))?; // with tools
for tc in resp.tool_calls {
    // run tc.name / tc.arguments locally, append the result, call complete again
}
```

The host translates the tool JSON into the provider's format; the guest never sees API keys. A module that drives its own loop (LLM → local tools → LLM, bounded by a turn limit, with one tool-less fallback call at the end) is worked through step by step in [Tutorial: benford-agent](../start/tutorial-benford-agent.md); the SDK basics (echo, tools, logging, agent card) are in [Tutorial: echo-agent](../start/tutorial-echo-agent.md).

### 4. Build and install

```sh
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/my_agent.wasm .

# Linux example; macOS is ~/Library/Application Support/chatty/modules/, Windows %APPDATA%\chatty\modules\
mkdir -p ~/.local/share/chatty/modules/my-agent
cp -r . ~/.local/share/chatty/modules/my-agent/
```

In Chatty: **Settings → Modules** → enable modules, set the module directory if you used another path, then restart or reload.

### 5. Test your module

Unit tests for pure-Rust logic run on the host target with a plain `cargo test` inside the module directory. For the gateway round trip, the repo's echo-agent suite is the model to copy:

```sh
# From the repo root — builds echo-agent WASM if needed
make wasm-modules
cargo test -p chatty-protocol-gateway echo_agent
```

Manual smoke test with the gateway running:

```sh
curl -s http://localhost:8420/a2a/my-agent \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"message/send",
       "params":{"message":{"parts":[{"type":"text","text":"hello"}]}}}'
```

Then from a conversation: `/agent my-agent hello`.

## Verify

- `GET http://localhost:8420/` lists your module and its endpoints.
- The curl above returns your `chat` output in `result.message.parts`.
- `POST /mcp/my-agent` with `tools/list` shows the tools you advertised.
- `/agent my-agent …` in Chatty returns the same answer.

## Checklist

- [ ] `wasm32-wasip2` target installed
- [ ] `module.toml` `[module].name` matches the agent card `name` and the directory name
- [ ] `[protocols].a2a = true`
- [ ] `.wasm` copied next to `module.toml`
- [ ] Modules enabled in Settings, gateway reachable on `:8420`
- [ ] Host-target unit tests for your tool logic

## Common mistakes

| Mistake | Fix |
|---------|-----|
| Module missing from `list_agents` | `[protocols].a2a = true` |
| Built for the host target | Use the `.cargo/config.toml` from the template, or pass `--target wasm32-wasip2` |
| `.wasm` name does not match `[module].wasm` | The manifest path is relative to `module.toml` |
| Calling `llm::complete` with a model id Chatty does not have | Pass `""` for the host default |
| Expecting the host to run your tool calls | The host only services `llm::complete`; execute tools in the guest and call again |

## Reference

| Topic | Doc |
|-------|-----|
| Gateway routes, `invoke_agent` flow, manifest fields, resource limits, module directory per platform | [A2A and WASM modules](../architecture/a2a-and-wasm-modules.md) |
| WIT types, host imports, guest exports, versioning | [WIT reference](../architecture/wit-reference.md) |
| Crate stack diagram | [Component map](../architecture/component-map.md) |
| Template source | [`templates/module/`](https://github.com/boersmamarcel/chatty2/tree/main/templates/module) |
| Reference modules | [`modules/echo-agent/`](https://github.com/boersmamarcel/chatty2/tree/main/modules/echo-agent), [`modules/benford-agent/`](https://github.com/boersmamarcel/chatty2/tree/main/modules/benford-agent) |
| End-user: enabling modules | [Extensions & MCP](../../user/extensions.md) |
| SDK rustdoc | [`chatty-module-sdk`](https://github.com/boersmamarcel/chatty2/tree/main/crates/chatty-module-sdk) (`cargo doc -p chatty-module-sdk --open`) |
