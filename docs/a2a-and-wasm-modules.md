# A2A and WASM Module Architecture

> How Chatty communicates with agents — both remote A2A services and locally installed WASM modules.

---

## Overview

Chatty supports two kinds of agents that can be invoked during a conversation:

| Agent type | Where it runs | How it's called | Configured in |
|:-----------|:--------------|:----------------|:--------------|
| **Remote A2A** | External HTTP service | Direct HTTP to the remote URL | Settings → A2A Agents |
| **Local WASM module** | In-process via Wasmtime | Via the local Protocol Gateway (`localhost:8420`) | Settings → Modules |

Both agent types are **unified behind the same tools** (`list_agents`, `invoke_agent`) and the same **A2A JSON-RPC protocol**, so the LLM doesn't need to know which kind it's talking to.

```
┌────────────────────────────────────────────────────────────────────┐
│                           Chatty LLM                              │
│                                                                   │
│  list_agents → discovers both remote + local agents               │
│  invoke_agent("agent-name", "prompt") → unified invocation        │
│                                                                   │
├─────────────────────┬──────────────────────────────────────────────┤
│   Remote A2A path   │          Local WASM module path              │
│                     │                                              │
│   A2aClient ────────┤   A2aClient ──► Protocol Gateway ──► WASM   │
│     ↓               │     ↓           (localhost:8420)    module   │
│   HTTP POST to      │   HTTP POST to                              │
│   remote URL        │   /a2a/{module}                              │
└─────────────────────┴──────────────────────────────────────────────┘
```

---

## Remote A2A Agents

### Configuration

Remote agents are configured in **Settings → A2A Agents** and persisted to JSON via `A2aJsonRepository`.

**Data model** (`A2aAgentConfig` in `crates/chatty-core/src/settings/models/a2a_store.rs`):

```rust
pub struct A2aAgentConfig {
    pub name: String,           // User-visible name, also the invocation key
    pub url: String,            // Base URL (e.g. "https://hive.dev/a2a/voucher-agent")
    pub api_key: Option<String>,// Optional Bearer token
    pub enabled: bool,          // Toggle on/off
    pub skills: Vec<String>,    // Cached from agent card discovery
}
```

Runtime connection status is tracked in `A2aAgentsModel` (a GPUI global) but **not persisted** — it's refreshed at startup by fetching agent cards.

### Protocol

Remote A2A agents implement the [A2A protocol](https://google.github.io/A2A/). Chatty acts as an **A2A client** (`crates/chatty-core/src/services/a2a_client.rs`).

#### Agent Card Discovery

```
GET <base_url>/.well-known/agent.json
Authorization: Bearer <api_key>   (if configured)
```

Response fields parsed by Chatty:

| Field | Usage |
|:------|:------|
| `name` / `displayName` | Agent name |
| `description` | Shown in agent list |
| `skills[].name` | Cached in `A2aAgentConfig.skills` |
| `capabilities.streaming` | Whether `message/stream` is supported |

#### Sending Messages

**Non-streaming** (`message/send`):

```json
POST <base_url>
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "message/send",
  "params": {
    "message": { "parts": [{ "type": "text", "text": "<prompt>" }] },
    "taskId": "<uuid>"
  }
}
```

Response text is extracted from `result.artifacts[0].parts[0].text`.

**Streaming** (`message/stream`):

Same request body but with `"method": "message/stream"`. The response is an SSE (`text/event-stream`) byte stream. Each SSE event contains a JSON-RPC result:

```
data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-123","status":{"state":"working"},"final":false}}

data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-123","artifact":{"parts":[{"type":"text","text":"Hello"}],"index":0,"lastChunk":false}}}

data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-123","status":{"state":"completed"},"final":true}}
```

Events are parsed into `A2aStreamEvent`:

| Variant | When |
|:--------|:-----|
| `StatusUpdate { state: "working", message }` | Agent is processing; optional progress text |
| `ArtifactUpdate { text, last_chunk }` | A chunk of the agent's response |
| `StatusUpdate { state: "completed", is_final: true }` | Terminal — stream ends |
| `StatusUpdate { state: "failed", message }` | Terminal — error |

If the server responds with `Content-Type` other than `text/event-stream`, the client **falls back** to treating it as a non-streaming `message/send` response.

---

## Local WASM Module Agents

### Architecture Stack

```
┌──────────────────────────────────────────────────────────┐
│                    chatty-module-sdk                      │
│  (Rust SDK for module authors, targets wasm32-wasip2)    │
│  Provides: ModuleExports trait, export_module! macro,    │
│            llm::complete, config::get, log::info, etc.   │
├──────────────────────────────────────────────────────────┤
│                   chatty-wasm-runtime                     │
│  (Wasmtime-based host: loads .wasm, implements imports,  │
│   calls guest exports with fuel/memory/timeout limits)   │
├──────────────────────────────────────────────────────────┤
│                  chatty-module-registry                   │
│  (Discovers modules on disk, parses module.toml,         │
│   manages load/unload/hot-reload lifecycle)              │
├──────────────────────────────────────────────────────────┤
│                 chatty-protocol-gateway                   │
│  (HTTP server exposing modules via OpenAI, MCP, and A2A  │
│   protocols simultaneously on localhost:8420)            │
└──────────────────────────────────────────────────────────┘
```

### WIT Contract

The host–guest interface is defined in [`wit/chatty-module.wit`](../wit/chatty-module.wit) (package `chatty:module@0.1.0`). See [`docs/wit-reference.md`](wit-reference.md) for the full type reference.

**Host imports** (what the host provides to the module):

| Interface | Function | Purpose |
|:----------|:---------|:--------|
| `llm` | `complete(model, messages, tools)` | Run an LLM completion via host-managed API keys |
| `config` | `get(key)` | Read key-value config from the module's manifest |
| `logging` | `log(level, message)` | Emit structured logs to the host's log output |

**Guest exports** (what the module provides to the host):

| Interface | Function | Purpose |
|:----------|:---------|:--------|
| `agent` | `chat(req) → response` | Handle a conversation turn |
| `agent` | `invoke-tool(name, args) → result` | Execute a module-provided tool |
| `agent` | `list-tools() → definitions` | Enumerate available tools |
| `agent` | `get-agent-card() → card` | Return metadata (name, description, skills) |

### Module Directory Layout

```
~/Library/Application Support/chatty/modules/   # macOS default
├── echo-agent/
│   ├── module.toml          # Manifest (required)
│   └── echo_agent.wasm      # WASM component binary
└── code-reviewer/
    ├── module.toml
    └── code_reviewer.wasm
```

The directory is configurable in **Settings → Modules**. Platform defaults:

| Platform | Path |
|:---------|:-----|
| macOS | `~/Library/Application Support/chatty/modules/` |
| Linux | `~/.local/share/chatty/modules/` (or `$XDG_DATA_HOME/chatty/modules/`) |
| Windows | `%APPDATA%\chatty\modules\` |

### Module Manifest (`module.toml`)

```toml
[module]
name = "echo-agent"
version = "0.1.0"
description = "A simple echo agent for testing"
wasm = "echo_agent.wasm"        # Relative to this file's directory

[capabilities]
tools = ["echo", "reverse"]     # Tool names the module exposes
chat = true                     # Implements the chat export
agent = true                    # Acts as an autonomous agent

[protocols]
openai_compat = true            # Expose via /v1/{name}/chat/completions
mcp = true                      # Expose via /mcp/{name}
a2a = true                      # Expose via /a2a/{name} (required for invoke_agent)

[resources]
max_memory_mb = 64              # Memory cap (0 = use default: 64 MiB)
max_execution_ms = 30000        # Timeout (0 = use default: 300s)
```

The `[protocols].a2a = true` flag is what makes a module invocable as an agent from conversations. Without it, the module can still serve tools via MCP or completions via OpenAI-compat, but won't appear in `list_agents` output.

### Resource Limits

Every WASM module runs inside a sandboxed Wasmtime instance with three enforcement mechanisms:

| Limit | Default | Purpose |
|:------|:--------|:--------|
| **Fuel** | 100,000,000 units | CPU budget (≈1 unit per Wasm instruction) |
| **Memory** | 64 MiB | Linear memory cap |
| **Timeout** | 300,000 ms (5 min) | Wall-clock execution limit |

These can be overridden per-module via `module.toml [resources]`.

### Module SDK

Module authors use `chatty-module-sdk` (targets `wasm32-wasip2`):

```rust
use chatty_module_sdk::*;

#[derive(Default)]
struct MyAgent;

impl ModuleExports for MyAgent {
    fn chat(&self, req: ChatRequest) -> Result<ChatResponse, String> {
        let resp = llm::complete("claude-sonnet-4-20250514", &req.messages, None)?;
        Ok(ChatResponse {
            content: resp.content,
            tool_calls: vec![],
            usage: resp.usage,
        })
    }

    fn invoke_tool(&self, _name: String, _args: String) -> Result<String, String> {
        Err("no tools".into())
    }

    fn list_tools(&self) -> Vec<ToolDefinition> { vec![] }

    fn get_agent_card(&self) -> AgentCard {
        AgentCard {
            name: "my-agent".into(),
            display_name: "My Agent".into(),
            description: "Does something useful".into(),
            version: "0.1.0".into(),
            skills: vec![], tools: vec![],
        }
    }
}

export_module!(MyAgent);
```

Build with: `cd crates/chatty-module-sdk && cargo build` (uses `.cargo/config.toml` to target `wasm32-wasip2`).

---

## Protocol Gateway

The **Protocol Gateway** (`chatty-protocol-gateway`) is a local HTTP server that exposes all loaded WASM modules through three protocols simultaneously:

```
http://localhost:8420/
```

### Routes

| Method | Path | Protocol | Description |
|:-------|:-----|:---------|:------------|
| `GET` | `/` | — | JSON index of all modules and endpoints |
| `GET` | `/.well-known/agent.json` | A2A | Aggregated agent card (all modules) |
| `GET` | `/a2a/{module}/.well-known/agent.json` | A2A | Per-module agent card |
| `POST` | `/a2a/{module}` | A2A | JSON-RPC: `message/send`, `message/stream`, `tasks/get` |
| `POST` | `/v1/{module}/chat/completions` | OpenAI | Module-specific chat completion |
| `POST` | `/v1/chat/completions` | OpenAI | Model-routed (`model: "module:{name}"`) |
| `POST` | `/mcp/{module}` | MCP | JSON-RPC: `tools/list`, `tools/call`, `initialize` |
| `GET` | `/mcp/{module}/sse` | MCP | SSE transport |

### A2A via the Gateway

When a local WASM module agent is invoked, the `invoke_agent` tool constructs an `A2aAgentConfig` pointing at the gateway:

```rust
// invoke_agent_tool.rs — local module path
let config = A2aAgentConfig {
    name: agent_name,
    url: format!("http://localhost:8420/a2a/{}", agent_name),
    api_key: None,
    enabled: true,
    skills: module.tools.clone(),
};
// Then calls self.call_streaming(&config, &prompt)
```

This means **the exact same `A2aClient` code path handles both remote and local agents**. The only difference is the URL.

### Gateway A2A Streaming

The gateway's `message/stream` handler (`handlers/a2a.rs`):

1. Emits `{"status": {"state": "working"}, "final": false}` immediately
2. Spawns the module's `chat()` call in a background task
3. Forwards module `logging::log()` calls as real-time progress events via an `mpsc` channel
4. On completion, emits the artifact (`parts[0].text`) and a final `{"status": {"state": "completed"}, "final": true}`
5. On error, emits `{"status": {"state": "failed"}, "final": true}`

This means module authors can emit progress by calling `log::info("Processing step 3...")` — these messages appear in real-time in the SSE stream.

### Aggregated Agent Card

`GET /.well-known/agent.json` returns a gateway-level card listing all loaded module agents:

```json
{
  "schema_version": "0.1",
  "gateway": true,
  "agents": [
    {
      "name": "echo-agent",
      "displayName": "Echo Agent",
      "description": "...",
      "version": "0.1.0",
      "skills": [...],
      "capabilities": { "streaming": true }
    }
  ]
}
```

---

## LLM-Facing Tools

The LLM interacts with agents through two tools that are **always available** in every conversation:

### `list_agents`

Returns a combined view of both agent types:

```json
{
  "remote_agents": [
    { "name": "voucher-agent", "url": "https://...", "has_api_key": true, "enabled": true, "skills": ["..."] }
  ],
  "local_agents": [
    { "name": "echo-agent", "version": "0.1.0", "description": "...", "tools": ["echo"], "supports_a2a": true }
  ],
  "total": 2,
  "note": "To invoke an agent, use the `invoke_agent` tool..."
}
```

Key details:
- API key values are **never exposed** to the LLM — only `has_api_key: true/false`
- `supports_a2a` indicates whether a local module has `[protocols].a2a = true`
- Remote agents take precedence if a remote and local agent share the same name

### `invoke_agent`

Invokes an agent by name with a prompt. Handles both remote and local agents transparently:

```json
{ "agent": "echo-agent", "prompt": "Hello, agent!" }
```

Resolution order:
1. **Remote A2A agents** — checked first (precedence)
2. **Local WASM module agents** — checked second, requires `supports_a2a = true` and the gateway to be running

Both paths use `A2aClient::send_message_stream()` for real-time progress. Progress events (`InvokeAgentProgress`) are emitted to the UI stream loop so the user sees intermediate output.

---

## End-to-End Communication Flow

### Remote A2A Agent Invocation

```
User message → LLM decides to call invoke_agent("remote-agent", "task")
  → InvokeAgentTool.call()
    → finds A2aAgentConfig by name (remote_agents list)
    → A2aClient::send_message_stream(config, prompt)
      → POST https://remote-service.com/
        { "jsonrpc":"2.0", "method":"message/stream", "params":{...} }
      → SSE stream of A2aStreamEvents
        → StatusUpdate(working) → progress to UI
        → ArtifactUpdate(text)  → progress to UI, accumulate response
        → StatusUpdate(completed, final=true) → stream ends
    → InvokeAgentOutput { response, success: true }
  → LLM receives tool result, continues conversation
```

### Local WASM Module Agent Invocation

```
User message → LLM decides to call invoke_agent("echo-agent", "task")
  → InvokeAgentTool.call()
    → not found in remote_agents
    → found in module_agents, supports_a2a = true
    → constructs A2aAgentConfig { url: "http://localhost:8420/a2a/echo-agent" }
    → A2aClient::send_message_stream(config, prompt)
      → POST http://localhost:8420/a2a/echo-agent
        { "jsonrpc":"2.0", "method":"message/stream", "params":{...} }
      → Gateway receives request
        → Looks up "echo-agent" in ModuleRegistry
        → Emits SSE "working" event
        → Calls WasmModule::chat(ChatRequest { messages, conversation_id })
          → Wasmtime executes guest WASM
            → Guest may call llm::complete() → host LlmProvider → real LLM API
            → Guest may call config::get() → reads from ModuleManifest
            → Guest may call logging::log() → forwarded as SSE progress events
          → Guest returns ChatResponse { content, tool_calls, usage }
        → Gateway emits SSE artifact event with response content
        → Gateway emits SSE "completed" + final=true
      → A2aClient parses SSE → A2aStreamEvents → progress to UI
    → InvokeAgentOutput { response, success: true }
  → LLM receives tool result, continues conversation
```

### Key Design Principle

Local WASM modules are **not called directly** from the LLM tool layer. Instead, they are always accessed through the Protocol Gateway's A2A endpoint. This means:

1. **Single code path** — `InvokeAgentTool` uses `A2aClient` for both remote and local agents
2. **Protocol compliance** — local modules speak the same A2A protocol as remote services
3. **Multi-protocol exposure** — the same module is simultaneously available via OpenAI-compat, MCP, and A2A
4. **External access** — other tools and services on the machine can also call local modules via `localhost:8420`

---

## Sub-Agent Tool (Separate Mechanism)

The `sub_agent` tool is a **different mechanism** from A2A agent invocation. It spawns `chatty-tui` in headless mode as a subprocess:

```
sub_agent(task, model?) → chatty-tui --headless --model <model> --message <task>
```

This gives the sub-agent access to the **full Chatty tool set** (shell, file operations, MCP tools, etc.) but runs in a separate process with its own conversation context. It does not use the A2A protocol.

| Feature | `invoke_agent` | `sub_agent` |
|:--------|:---------------|:------------|
| Protocol | A2A (JSON-RPC over HTTP) | Process spawning |
| Target | Named remote/local agents | Another Chatty instance |
| Tool access | Agent's own tools only | Full Chatty tool set |
| Model | Agent's own model | Can override parent model |
| Streaming | SSE with progress events | stdout on completion |

---

## Crate Responsibilities

| Crate | Role |
|:------|:-----|
| `chatty-module-sdk` | Guest-side SDK for module authors (types, host import wrappers, `export_module!` macro) |
| `chatty-wasm-runtime` | Wasmtime host: loads `.wasm` components, implements host imports (`llm`, `config`, `logging`), enforces resource limits |
| `chatty-module-registry` | Discovery (`scan_directory`), lifecycle (`load`/`unload`/`reload`/`watch`), manifest parsing |
| `chatty-protocol-gateway` | HTTP server (axum) exposing modules via OpenAI, MCP, and A2A protocols |
| `chatty-core` | A2A client (`A2aClient`), agent tools (`list_agents`, `invoke_agent`), settings models and repositories |

---

## Settings Summary

### A2A Agents (Settings → A2A Agents)

| Field | Description |
|:------|:------------|
| Name | Invocation key and display name |
| URL | Base URL of the remote A2A service |
| API Key | Optional Bearer token (stored but never exposed to LLM) |
| Enabled | Toggle agent on/off |

### Modules (Settings → Modules)

| Field | Default | Description |
|:------|:--------|:------------|
| Enabled | `false` | Master toggle for the module runtime and gateway |
| Module Directory | Platform-specific (see above) | Where to scan for module subdirectories |
| Gateway Port | `8420` | TCP port for the local protocol gateway |
