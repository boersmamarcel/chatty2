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

**A question goes up the chain, the answer comes back down** (ADR-0011 C7,
AGE-306). A worker whose `ask_user` is waiting parks its task in
`input-required` and says what it is waiting for — the request id and every
question with its options — under `input`. The broker serves that to the A2A
caller under the status's `metadata.clarification`; the caller answers with
A2A `message/send` carrying the task's id on the message and the answers under
the message's `metadata.clarification`, which the broker turns into an `input`
frame on the same task. The worker's next status un-parks it.

```
participant → {"type":"status","taskId":"task-…","state":"input-required","message":"Which database?",
               "input":{"id":"req-…","questions":[{"id":"q1","question":"Which database?","options":["Postgres","SQLite"]}]}}
broker      → {"type":"input","taskId":"task-…","input":{"requestId":"req-…","answers":[{"id":"q1","answer":"Postgres","custom":false}]}}
participant → {"type":"status","taskId":"task-…","state":"working","message":"✓ ask_user"}
```

`invoke_agent` is the caller: it re-asks the question on its own agent's
`ask_user` store, so a human behind it sees the ordinary popover, and an agent
that is itself a worker parks its own task the same way — the question climbs
until it reaches someone who can answer, and the answer descends the same
hops. Escalate-to-human is the only policy; whether a leader may answer on a
worker's behalf is an open question on ADR-0011. A level with nobody to ask
ends the delegation rather than guessing. `message/send` on a fresh task cannot carry the question
to its caller, so a task parked under it waits out the worker's own
clarification timeout.

**The connection is the liveness signal.** There is no heartbeat: when the
socket closes, for any reason, the participant is deregistered and every task
it still owed is failed with a `failed` status naming the disconnect. A
process that has died cannot fail to send a heartbeat, so the socket is the
only signal that cannot lie.

Opt in with `ProtocolGateway::with_participant_socket(path)`; without it no
socket is opened. The hosted transport is Firecracker vsock, which reaches
this crate as a plain stream — `serve_connection` takes any of them, and
`ParticipantConnection::register_over` is the worker's side of the same
generalization (AGE-307).

### Virtual agents and the local runner

`ProtocolGateway::with_virtual_agent` publishes one agent that is not a
connected process but a factory. A task addressed to it starts a worker, waits
for that worker to register over the socket, routes the task to it, and reaps
it. To the caller it is an A2A agent like any other, which is the point:
`invoke_agent` replaces `sub_agent` without the parent learning a second
fan-out path.

`LocalRunner` is the implementation that spawns a `chatty-tui` child. It is
not the only one: hive's `VmRunner` leases a Firecracker microVM per task
(AGE-307) and fills the same slot, which is why the trait exists rather than
the gateway naming a concrete runner. Everything past "the worker registered"
is the same code for both.

**One child per task.** The child can serve tasks until its socket closes, but
the runner's policy is one-shot, keeping the process lifecycle identical to
the `sub_agent` it replaces — that equivalence is what makes ADR-0011's second
kill criterion a comparison of the hop rather than of process-reuse
strategies. Cancellation of a running task is enforced by reaping the child;
a persistent worker is a change to `runner.rs` and nothing else.

**Where a worker runs** is the embedder's decision, not the gateway's.
ADR-0012 gives each worker a `git worktree`, and git lives in `chatty-core`,
so the runner takes a `WorkspaceFactory` and only spawns in whatever directory
it is handed. Without a factory the child inherits the broker's own directory.
A factory that is configured and then *fails* fails the task rather than
silently running the worker unisolated.

## Running

```sh
cargo run -p chatty-protocol-gateway -- --modules-dir ~/.local/share/chatty/modules
```

The server binds to `http://0.0.0.0:8420` by default.

## Being a worker (`worker` feature)

The `worker` feature adds the other end of the participant socket: `TaskMapper`,
the `SessionEvent` → A2A table, and `serve_one_task`, the loop a process runs
when it *is* the worker — register, take one task, run it, send one terminal
status, exit.

It lives here rather than in `chatty-tui` because two crates run it:
`chatty-tui` on the desktop and hive's `chatty-server` inside a microVM. The
frame sequence a parent renders is what ADR-0011's first kill criterion is
measured on, so a second copy of the mapping would be a second answer to the
question the ADR asks. The feature is off by default — it is the only thing in
this crate that needs `chatty-core`, and a broker that never runs an agent
itself builds without it.
