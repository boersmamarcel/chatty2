# chatty-protocol-gateway

HTTP server that exposes agents through three protocol surfaces
simultaneously. All three use **plain HTTP + JSON over TCP** — there is no
gRPC, no WebSocket (except MCP SSE), and no binary framing.

An agent behind those surfaces is either a **loaded WASM module** or a
**local participant**: a process that registered over a Unix socket and is
served at the same `/a2a/{name}` routes (ADR-0011). The participant socket is
the one thing here that is not HTTP; see [Local participants](#local-participants).

## Transport

> **No gRPC.** All endpoints are plain HTTP requests with JSON bodies.

| Protocol | Method | Content-Type |
|----------|--------|-------------|
| OpenAI completions | `POST /v1/{module}/chat/completions` | `application/json` |
| MCP JSON-RPC | `POST /mcp/{module}` | `application/json` |
| MCP SSE stream | `GET /mcp/{module}/sse` | `text/event-stream` |
| A2A JSON-RPC | `POST /a2a/{module}` | `application/json` |
| A2A streaming | `POST /a2a/{module}` (method: `message/stream`) | `text/event-stream` |
| Agent card (per module) | `GET /a2a/{module}/.well-known/agent.json` | `application/json` |
| Agent card (aggregated) | `GET /.well-known/agent.json` | `application/json` |
| Participant registration | Unix socket (`with_participant_socket`) | newline-delimited JSON |

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

Speaks the A2A JSON-RPC 2.0 schema (`message/send`, `message/stream`,
`tasks/get`). Like the Completion API, the agentic loop runs behind the
gateway — inside the WASM module, or inside the participant process.

**`message/send`** returns a complete JSON-RPC response:

```json
{ "result": { "id": "task-…", "status": { "state": "completed" }, "artifacts": [{ "parts": [{ "type": "text", "text": "…" }] }] } }
```

**`message/stream`** returns an SSE stream (`text/event-stream`) with
incremental updates per the [A2A streaming spec](https://a2a-protocol.org/latest/topics/streaming-and-async/):

```
data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-…","status":{"state":"working"},"final":false}}

data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-…","artifact":{"parts":[{"type":"text","text":"…"}],"index":0,"lastChunk":true}}}

data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-…","status":{"state":"completed"},"final":true}}
```

The agent card advertises `"capabilities": { "streaming": true }` so clients
can discover streaming support.

> **Note:** The underlying WASM `chat` export is currently request/response,
> so the gateway emits the full response as a single artifact chunk. When the
> WIT interface gains a streaming chat export, this handler will emit
> finer-grained token-level events without changing the SSE wire format.

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
                          │            │                 │
                          │            ▼                 │
                          │ a2a_participant.rs           ├──► ParticipantRegistry
                          └──────────────────────────────┘         ▲
                                                                   │ Unix socket
                                                          child processes
```

The gateway holds a single `ModuleRegistry` (behind an `Arc<RwLock<…>>`) and
a single `ParticipantRegistry`. The module handlers call the WIT exports:

| Handler | WIT export called |
|---------|-------------------|
| OpenAI  | `agent::chat`     |
| MCP     | `agent::list-tools`, `agent::invoke-tool` |
| A2A `message/send` | `agent::chat` |
| A2A `message/stream` | `agent::chat` (SSE wrapper) |

## Local participants

A process that connects to the participant socket, publishes an agent card
and answers tasks is addressable at `/a2a/{name}` exactly like a module —
same JSON-RPC methods, same SSE frames, so an A2A client cannot tell the two
apart. **Participants are looked up first**, so a live process shadows a
module of the same name.

The socket carries newline-delimited JSON, not A2A: A2A is the gateway's
public wire format, and a child process is not a public endpoint.

```
participant → {"type":"register","card":{"name":"worker-1",…}}
broker      → {"type":"registered","name":"worker-1"}
broker      → {"type":"task","taskId":"task-…","text":"summarise foo.rs"}
participant → {"type":"status","taskId":"task-…","state":"working","message":"read_file"}
participant → {"type":"artifact","taskId":"task-…","text":"foo.rs defines…","lastChunk":false}
participant → {"type":"status","taskId":"task-…","state":"completed"}
```

`status` states are A2A's (`submitted`, `working`, `input-required`,
`completed`, `failed`, `canceled`); the terminal three end the task.

**The connection is the liveness signal.** There is no heartbeat: when the
socket closes, for any reason, the participant is deregistered and every task
it still owed is failed with a `failed` status naming the disconnect. A
process that has died cannot fail to send a heartbeat, so the socket is the
only signal that cannot lie.

Opt in with `ProtocolGateway::with_participant_socket(path)`; without it no
socket is opened. Unix only — the hosted transport is vsock (AGE-307).

## Running

```sh
cargo run -p chatty-protocol-gateway -- --modules-dir ~/.local/share/chatty/modules
```

The server binds to `http://0.0.0.0:8420` by default.
