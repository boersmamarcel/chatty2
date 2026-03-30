# chatty-protocol-gateway

HTTP server that exposes loaded WASM modules through three protocol surfaces
simultaneously. All three use **plain HTTP + JSON over TCP** — there is no
gRPC, no WebSocket (except MCP SSE), and no binary framing.

## Transport

> **No gRPC.** All endpoints are plain HTTP requests with JSON bodies.

| Protocol | Method | Content-Type |
|----------|--------|-------------|
| OpenAI completions | `POST /v1/{module}/chat/completions` | `application/json` |
| MCP JSON-RPC | `POST /mcp/{module}` | `application/json` |
| MCP SSE stream | `GET /mcp/{module}/sse` | `text/event-stream` |
| A2A JSON-RPC | `POST /a2a/{module}` | `application/json` |
| Agent card (per module) | `GET /a2a/{module}/.well-known/agent.json` | `application/json` |
| Agent card (aggregated) | `GET /.well-known/agent.json` | `application/json` |

## Protocol summary

### 1 · OpenAI Completion API

Speaks the OpenAI `POST /v1/chat/completions` shape. The full agentic loop
(LLM ↔ tools) runs inside the WASM module. The caller receives a finished
response in `choices[0].message.content` — intermediate tool calls are hidden.

### 2 · MCP (Model Context Protocol)

Speaks JSON-RPC 2.0 (`tools/list`, `tools/call`). There is **no** agentic
loop on the gateway side — each call is a direct pass-through to the module's
`list_tools` or `invoke_tool` WIT exports. The caller (an orchestrator or
another LLM) decides when to call each tool and how to interpret the raw JSON
output.

### 3 · A2A (Agent-to-Agent)

Speaks the A2A JSON-RPC 2.0 schema (`message/send`, `tasks/get`). Like the
Completion API, the full agentic loop runs inside the WASM module. The
response is wrapped in an A2A `message` envelope with typed `parts`:

```json
{ "result": { "message": { "role": "agent", "parts": [{ "type": "text", "text": "…" }] } } }
```

A2A differs from the Completion API only in the JSON envelope — the
underlying computation is identical.

## Architecture

```
                          ┌──────────────────────────────┐
HTTP client               │   chatty-protocol-gateway     │
                          │   (Axum HTTP server)          │
                          │                              │
POST /v1/{m}/chat/…  ────►│ openai.rs handler            │
POST /mcp/{m}        ────►│ mcp.rs handler               ├──► ModuleRegistry
POST /a2a/{m}        ────►│ a2a.rs handler               │    (wasmtime instances)
GET  /.well-known/…  ────►│ a2a.rs handler               │
                          └──────────────────────────────┘
```

The gateway holds a single `ModuleRegistry` (behind an `Arc<RwLock<…>>`).
All three handlers call the same underlying WIT exports:

| Handler | WIT export called |
|---------|-------------------|
| OpenAI  | `agent::chat`     |
| MCP     | `agent::list-tools`, `agent::invoke-tool` |
| A2A     | `agent::chat`     |

## Running

```sh
cargo run -p chatty-protocol-gateway -- --modules-dir ~/.local/share/chatty/modules
```

The server binds to `http://0.0.0.0:8420` by default.
