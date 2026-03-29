# echo-agent

Reference chatty WASM module — the canonical quickstart for module authors.

This module is both:

* **Reference implementation** — shows every SDK feature in ~130 lines.
* **End-to-end integration test** — the CI builds and runs the full
  protocol-gateway test suite against it.

---

## What it does

| Feature | Behaviour |
|---------|-----------|
| **chat** | Echoes the last user message prefixed with `"Echo: "`. If the message contains `"use llm"`, calls the host LLM completion import instead. |
| **tools** | `echo` — returns input unchanged · `reverse` — reverses characters · `count_words` — returns word count |
| **agent card** | name `"echo-agent"`, skill `"echoing"` |
| **logging** | Uses `chatty_module_sdk::log::*` at info/debug/warn/error levels |

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
cd modules/echo-agent
cargo build --target wasm32-wasip2 --release

# Copy the WASM to the module directory so the registry can find it
cp target/wasm32-wasip2/release/echo_agent.wasm .
```

The resulting `echo_agent.wasm` should be under 1 MB.

### Verify

```sh
# Check file size
ls -lh echo_agent.wasm

# Inspect the component model exports (requires wasm-tools)
wasm-tools component wit echo_agent.wasm
```

---

## Project layout

```
modules/echo-agent/
├── Cargo.toml          # cdylib, standalone [workspace]
├── .cargo/config.toml  # sets default target to wasm32-wasip2
├── module.toml         # registry manifest (name, version, wasm path, …)
├── src/
│   └── lib.rs          # ModuleExports implementation + export_module! macro
└── README.md           # this file
```

---

## How it works

The SDK exposes three layers:

### 1. Types

```rust
use chatty_module_sdk::{
    AgentCard, ChatRequest, ChatResponse, Role, Skill, ToolDefinition,
};
```

These are Rust re-exports of the WIT types defined in `wit/chatty-module.wit`.

### 2. Host imports

```rust
// Call the host-managed LLM
let resp = chatty_module_sdk::llm::complete("", &messages, None)?;

// Read a config value set in module.toml / registry
let val = chatty_module_sdk::config::get("my-key");

// Structured logging (forwarded to tracing on the host)
chatty_module_sdk::log::info("hello from wasm");
```

### 3. Trait + macro

```rust
#[derive(Default)]
pub struct MyAgent;

impl ModuleExports for MyAgent {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> { ... }
    fn invoke_tool(&self, name: String, args: String) -> Result<String, String> { ... }
    fn list_tools(&self) -> Vec<ToolDefinition> { ... }
    fn get_agent_card(&self) -> AgentCard { ... }
}

export_module!(MyAgent);   // wires trait → WIT guest exports
```

---

## Building your own module

Use the cargo-generate template from the repository root:

```sh
cargo generate --path templates/module --name my-agent
cd my-agent
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/my_agent.wasm .
```

Then copy the directory into your chatty modules folder and restart the
registry.

---

## Running the end-to-end tests

After building the WASM (see [Build](#build)):

```sh
# From the workspace root
cargo test -p chatty-protocol-gateway echo_agent
```

The test suite covers all 12 integration steps:

1. Module registry discovers and loads echo-agent
2. `list_tools()` returns 3 tools
3. `invoke_tool("echo", "hello")` → `"hello"`
4. `invoke_tool("reverse", "hello")` → `"olleh"`
5. `chat(messages)` → `"Echo: …"`
6. `agent_card()` → name + skills verified
7. `GET /.well-known/agent.json` → echo-agent listed
8. `POST /mcp/echo-agent` `tools/list` → JSON-RPC response with 3 tools
9. `POST /v1/echo-agent/chat/completions` → OpenAI-format echo response
10. `POST /a2a/echo-agent` `message/send` → A2A response
