# chatty-protocol-gateway

HTTP server with two protocol surfaces, both **plain HTTP + JSON over TCP** —
there is no gRPC, no WebSocket (except MCP SSE), and no binary framing:

- **MCP** serves the tools of each loaded WASM plugin (`chatty:plugin@0.3.0`)
  to external MCP clients. A plugin has tools and no loop of its own, so it
  is never an agent and has no other route (PL-U3).
- **A2A** serves agents: **local participants** (worker processes the broker
  spawned on a connection it made for them) and **virtual agents** (ADR-0011,
  ADR-0020). That connection is the one thing here that is not HTTP; see
  [Local participants](#local-participants).

## Transport

> **No gRPC.** All endpoints are plain HTTP requests with JSON bodies.

| Protocol | Method | Content-Type |
|----------|--------|-------------|
| MCP JSON-RPC | `POST /mcp/{module}` | `application/json` |
| MCP SSE stream | `GET /mcp/{module}/sse` | `text/event-stream` |
| MCP SSE message | `POST /mcp/{module}/sse?sessionId=…` | `application/json` |
| A2A JSON-RPC | `POST /a2a/{agent}` | `application/json` |
| A2A streaming | `POST /a2a/{agent}` (method: `message/stream`) | `text/event-stream` |
| Agent card (per agent) | `GET /a2a/{agent}/.well-known/agent.json` | `application/json` |
| Agent card (aggregated) | `GET /.well-known/agent.json` | `application/json` |
| Participant connection | broker-made `socketpair` per worker (`open_connection`) | newline-delimited JSON, v2 |

### What every route enforces

- **Loopback callers only.** A request whose `Host` is not a loopback name
  (`localhost`, `127.0.0.0/8`, `[::1]`), or whose `Origin` is present and not
  loopback (`null` included), gets **403**. Binding to 127.0.0.1 does not stop
  DNS rebinding: a browser page that re-resolves its own name to 127.0.0.1
  reaches the socket, and from there every module's `llm::complete` and every
  virtual agent. A request without `Host` (not a browser's) is served.
- **Request bodies up to 10 MiB** (`MAX_REQUEST_BYTES`); a larger one is a
  **413** before any handler runs.
- **`[protocols] mcp`.** A plugin is served over MCP only when its manifest
  sets `mcp = true`; otherwise it answers **404**, as if it were not loaded.
  It is never on the aggregated agent card.
- **One lock per module.** The registry is locked only to look a module up;
  the guest call runs under that module's own lock (`ModuleHandle`), on the
  blocking pool. Calls to one module queue; calls to different modules do
  not wait on each other. Credit checks happen before the module lock.
- **Guest output is capped** at 1 MiB per call by the runtime (PL-D3); a
  reply over the cap is a **502** with a short error body, never relayed.

## Protocol summary

### 1 · MCP (Model Context Protocol)

Speaks JSON-RPC 2.0 (`tools/list`, `tools/call`). Each call is a direct
pass-through to the plugin's `list-tools` or `invoke-tool` WIT exports; the
caller (an orchestrator or another LLM) decides when to call each tool and
how to interpret its output. `tools/call` hands the guest its `arguments`
object JSON-encoded once, as `tool-call-request.arguments-json` — what the
tool's `inputSchema` describes.

Two transports, one dispatcher:

- **Streamable HTTP** — `POST /mcp/{module}`, each message answered in its
  response (what chatty's own rmcp client uses).
- **HTTP+SSE** (MCP 2024-11-05) — `GET /mcp/{module}/sse` opens a stream whose
  first event is `endpoint` (`/mcp/{module}/sse?sessionId=…`). The client POSTs
  each message there, gets `202 Accepted`, and the answer arrives on the stream
  as a `message` event. The stream stays open (keep-alives) until the client
  disconnects, which ends the session.

### 2 · A2A (Agent-to-Agent)

Speaks the A2A JSON-RPC 2.0 schema (`message/send`, `message/stream`,
`tasks/get`) for participants and virtual agents; the agent's loop runs in
its own process. Every text part of `message.parts` reaches the agent, joined
by newlines. A module whose Hive metadata says `execution_mode = "remote"` is
forwarded to the Hive runner until PL-H8b removes that path; a local plugin
has no A2A route.

**`message/send`** returns a complete JSON-RPC response:

```json
{ "result": { "id": "task-…", "contextId": "…", "status": { "state": "completed" }, "artifacts": [{ "parts": [{ "type": "text", "text": "…" }] }] } }
```

**`message/stream`** returns an SSE stream (`text/event-stream`) with
incremental updates per the [A2A streaming spec](https://a2a-protocol.org/latest/topics/streaming-and-async/):

```
data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-…","contextId":"…","status":{"state":"working"},"final":false}}

data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-…","contextId":"…","artifact":{"parts":[{"type":"text","text":"…"}],"index":0,"lastChunk":true}}}

data: {"jsonrpc":"2.0","id":1,"result":{"id":"task-…","contextId":"…","status":{"state":"completed"},"final":true}}
```

The agent card advertises `"capabilities": { "streaming": true }` so clients
can discover streaming support.

## Architecture

```
                          ┌──────────────────────────────┐
HTTP client               │   chatty-protocol-gateway     │
                          │   (Axum HTTP server)          │
                          │                              │
POST /mcp/{m}        ────►│ mcp.rs handler               ├──► ModuleRegistry
                          │                              │    (wasmtime instances)
POST /a2a/{agent}    ────►│ a2a.rs handler               │
GET  /.well-known/…  ────►│ a2a.rs handler               │
                          │            │                 │
                          │            ▼                 │
                          │ a2a_participant.rs           ├──► ParticipantRegistry
                          └──────────────────────────────┘         ▲
                                                                   │ socketpair per child
                                                          child processes
```

The gateway holds a single `ModuleRegistry` (behind an `Arc<RwLock<…>>`, read
only to look a module up) whose modules each sit behind their own lock, and a
single `ParticipantRegistry`. Only the MCP handler calls a plugin, through
its `plugin::list-tools` and `plugin::invoke-tool` exports.

## Local participants

A worker on a broker-made connection that publishes an agent card and
answers tasks is addressable at `/a2a/{name}`, with the same JSON-RPC methods
and SSE frames as any A2A agent.

**The connection is the identity** (ADR-0020). The broker admits a node —
`Directory::admit` names it `<spec>-<n>`, never reusing a name — creates a
`socketpair`, keeps one end and hands the other to the child it spawns at
descriptor 3 (`chatty-tui --participant-fd 3`; `open_connection`,
`LocalRunner`). The worker's `hello` names nothing: a card's `name` is
ignored, and the broker's `welcome` says who the worker is. There is no way
to register otherwise. The shared socket (`with_participant_socket`, `bind`,
`serve`) stays bound and refuses every connection with an `error` frame, so
no local process can take a name the broker is about to route a task to.

The connection carries newline-delimited JSON, not A2A: A2A is the gateway's
public wire format, and a child process is not a public endpoint. This is
**version 2**: every frame in both directions carries `"v":2`, and a frame
without it is answered with an `error` frame naming v2 and the connection is
closed. There is no v1 fallback. (hive's worker speaks v1 at its current
chatty2 pin and moves to v2 with HS-4, AGE-678.)

```
participant → {"v":2,"type":"hello","card":{"name":"",…}}
broker      → {"v":2,"type":"welcome","name":"local-coder-0","scope":"root","owner":null}
broker      → {"v":2,"type":"task","taskId":"task-…","text":"summarise foo.rs"}
participant → {"v":2,"type":"status","taskId":"task-…","state":"working","message":"read_file"}
participant → {"v":2,"type":"artifact","taskId":"task-…","text":"foo.rs defines…","lastChunk":false}
participant → {"v":2,"type":"status","taskId":"task-…","state":"completed"}
```

`scope` is the conversation the node works for and `owner` the node that
asked for it; for now every node is the root's, in scope `root`. The task
frame also carries the node's `spawnContext` (BI-5, below).

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
participant → {"v":2,"type":"status","taskId":"task-…","state":"input-required","message":"Which database?",
               "input":{"id":"req-…","questions":[{"id":"q1","question":"Which database?","options":["Postgres","SQLite"]}]}}
broker      → {"v":2,"type":"input","taskId":"task-…","input":{"requestId":"req-…","answers":[{"id":"q1","answer":"Postgres","custom":false}]}}
participant → {"v":2,"type":"status","taskId":"task-…","state":"working","message":"✓ ask_user"}
```

**Calls over the connection (ADR-0020, BI-4).** A worker's `invoke_agent` and
`list_agents` reach local roles and the directory over the same connection,
not over loopback HTTP. Each is a `call` with the worker's own `id`; the
broker runs it as the node the connection names and answers under that `id`,
so several calls can be in flight and finish in any order. Closing the
connection cancels every call still in flight on it, which reaps the workers
those calls started.

```
participant → {"v":2,"type":"call","id":1,"method":"invoke_agent","params":{"agent":"local-reviewer","prompt":"review it","handle":null,"include_trace":false}}
participant → {"v":2,"type":"call","id":2,"method":"list_agents"}
broker      → {"v":2,"type":"call_result","id":2,"result":[{"name":"local-reviewer","origin":"local",…}]}
broker      → {"v":2,"type":"call_progress","id":1,"event":{"Step":"read_file"}}
broker      → {"v":2,"type":"call_result","id":1,"result":{"success":true,"response":"Looks good.","metadata":{…}}}
```

A call that cannot run ends with `call_error` (`{"kind":"unknown_agent","message":…}`);
a callee whose task failed ends with a `call_result` whose `success` is false.
The in-process root uses `ProtocolGateway::transport()`, the same calls with no
socket.

**A callee's question comes back down the call** (BI-5). When a callee parks
its task, the broker tells the calling worker with `call_input_required`, and
the worker's answer goes up as `call_input`, in the `input` frame's shape. The
broker delivers it only to the task that call parked.

```
broker      → {"v":2,"type":"call_input_required","id":1,"task":"task-…","request":{"id":"req-…","questions":[…]}}
participant → {"v":2,"type":"call_input","id":1,"task":"task-…","input":{"requestId":"req-…","answers":[…]}}
```

**The spawn context rides the request** (ADR-0020 invariants 5–6, BI-5). Only
a root process runs a broker; a sub-leader delegates over its connection. A
worker a call starts is spawned with a `SpawnContext {workspace_root,
base_branch, roster, verification, endpoint}` the broker derives from the
calling node's own (its tree, its branch, its roster; the root's verification
command and endpoint for the agent), so a sub-leader's worker branches from the
sub-leader's branch. A call may bring one as `params.spawn_context`; it is
clamped to the caller's own and refused otherwise with `call_error`
`{"kind":"spawn_context_refused","message":{"field":…,"reason":…}}`
(`participant::spawn_context`).

`invoke_agent` is the caller: it re-asks the question on its own agent's
`ask_user` store, so a human behind it sees the ordinary popover, and an agent
that is itself a worker parks its own task the same way — the question climbs
until it reaches someone who can answer, and the answer descends the same
hops. Escalate-to-human is the only policy; whether a leader may answer on a
worker's behalf is an open question on ADR-0011. A level with nobody to ask
ends the delegation rather than guessing.

**A non-streaming caller gets the question as a failure** (AGE-321). A plain
`message/send` reply is a single object with no room for a non-terminal
update, so a worker that parks under one is asking someone who will never
hear it. The broker ends the task at that point and quotes the question in
`status.message`, leaving the structured request on `status.metadata`:

```json
{ "id": "task-…",
  "status": { "state": "failed",
              "message": { "parts": [{ "type": "text",
                "text": "the worker asked: Which database? — a `message/send` task cannot carry a question back to its caller…" }] },
              "metadata": { "clarification": { "id": "req-…", "questions": [ … ] } } } }
```

> **Decision, 2026-09-09 (Marcel).** Fail fast rather than hold the task open
> for `tasks/get` polling. Polling is the A2A-shaped answer and would make
> non-streaming callers first-class, but it makes the broker stateful for open
> tasks — a task table, its cleanup, and lifetimes that interact with leases
> and the ledger — and every delegation path in this repository streams, so
> the callers it would serve are third parties. Failing immediately with the
> question in hand removes the multi-minute silent hang that was the actual
> complaint, and costs nothing that polling would later have to undo.

**The connection is the liveness signal.** There is no heartbeat: when the
connection closes, for any reason, the participant is deregistered and every
task it still owed is failed with a `failed` status naming the disconnect. A
process that has died cannot fail to send a heartbeat, so the connection is
the only signal that cannot lie.

The hosted transport is Firecracker vsock, which reaches this crate as a
plain stream — `serve_connection(stream, registry, admitted_node)` takes any
of them, and `ParticipantConnection::hello_over` is the worker's side of the
same generalization (AGE-307; the hosted broker adopts it with HS-4).

### Virtual agents and the local runner

`ProtocolGateway::with_virtual_agent` publishes one agent that is not a
connected process but a factory. A task addressed to it starts a worker on a
connection the broker made for it, waits for that worker's `hello`, routes the
task to it, and reaps it. To the caller it is an A2A agent like any other, which is the point:
`invoke_agent` replaces `sub_agent` without the parent learning a second
fan-out path.

`LocalRunner` is the implementation that spawns a `chatty-tui` child. It is
not the only one: hive's `VmRunner` leases a Firecracker microVM per task
(AGE-307) and fills the same slot, which is why the trait exists rather than
the gateway naming a concrete runner. Everything past "the worker said hello"
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

This crate is a library, not a binary — it has no `[[bin]]` target and no
`--modules-dir` CLI. An embedder constructs a `ModuleRegistry`, calls
`scan_directory`, and passes it to `ProtocolGateway::new(registry, port)`.
The desktop (`chatty-gpui`) and `chatty-tui --broker` both do this; see their
module-settings / broker wiring for a worked example, or
`crates/chatty-protocol-gateway/tests/` for a minimal one.

The gateway binds to `127.0.0.1:<port>` — never `0.0.0.0` — on whatever port
the embedder passes to `ProtocolGateway::new`; the desktop defaults that port
to `8420`.

## Being a worker (`worker` feature)

The `worker` feature adds the other end of the participant connection:
`TaskMapper`, the `SessionEvent` → A2A table, and `serve_one_task`, the loop a
process runs when it *is* the worker — say hello, take one task, run it, send
one terminal status, exit.

It lives here rather than in `chatty-tui` because two crates run it:
`chatty-tui` on the desktop and hive's `chatty-server` inside a microVM. The
frame sequence a parent renders is what ADR-0011's first kill criterion is
measured on, so a second copy of the mapping would be a second answer to the
question the ADR asks. The feature is off by default — it is the only thing in
this crate that needs `chatty-core`, and a broker that never runs an agent
itself builds without it.
