# chatty-protocol-gateway

HTTP server with two protocol surfaces, both **plain HTTP + JSON over TCP** —
there is no gRPC, no WebSocket (except MCP SSE), and no binary framing:

- **MCP** serves the tools of each loaded WASM plugin (`chatty:plugin@0.4.0`)
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
| A2A JSON-RPC | `POST /a2a/{module}` | `application/json` |
| A2A streaming | `POST /a2a/{module}` (method: `message/stream`) | `text/event-stream` |
| Agent card (per module) | `GET /a2a/{module}/.well-known/agent.json` | `application/json` |
| Agent card (aggregated) | `GET /.well-known/agent.json` | `application/json` |
| Participant connection | broker-made `socketpair` per worker (`open_connection`) | newline-delimited JSON, v2 |

All of it is served on a Unix socket in an owner-only directory, never on a
TCP port (see [Running](#running)).

### What every route enforces

- **The launch token.** Every route, and the fallback, answers **401**
  without `Authorization: Bearer <token>`, where the token is the gateway's
  per-launch one (`ProtocolGateway::token()`, written to `gateway.token`
  beside the socket, `0600`; ADR-0021 § 4).
- **Loopback callers only.** A request whose `Host` is not a loopback name
  (`localhost`, `127.0.0.0/8`, `[::1]`), or whose `Origin` is present and not
  loopback (`null` included), gets **403**, a guard against DNS rebinding
  for any listener an embedder puts in front of the socket. A request
  without `Host` (not a browser's) is served.
- **The `/a2a/{name}` surface is modules and the remote-runner forward only
  (BI-7, ADR-0020).** Once a worker calls over its own connection (BI-4), a
  `{name}` that is a role — a registered participant or a virtual agent —
  or the aggregated card when this gateway has any role at all, is refused
  with **403** `{"error":"fabric: roles are reached over the worker
  connection"}`, logged as a refusal row on the edge log. There is no
  `loopback_roles` setting to bring the old path back: see [Local
  participants](#local-participants).
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
`tasks/get`), but only for the remote-runner forward now (BI-7, ADR-0020): a
module whose Hive metadata says `execution_mode = "remote"` is forwarded to
the Hive runner over this shape until PL-H8b removes that path. A local
plugin has no A2A route (PL-U3). A role — a registered participant or a
virtual agent — is refused here with **403**; it answers `message/send` and
`message/stream` over the connection the broker made for its caller instead
(see [Local participants](#local-participants)), which is where the frame
shapes below actually apply.

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
POST /a2a/{module}   ────►│ a2a.rs handler  ─────────────┼──► remote runner (PL-H8b)
GET  /.well-known/…  ────►│ a2a.rs handler  (403 once     │
                          │  this gateway has a role)     │
                          └──────────────────────────────┘

                          ┌──────────────────────────────┐
worker (child process,    │  socketpair per worker        │
 own connection)     ◄───►│  (open_connection, BrokerCalls)├──► ParticipantRegistry
                          └──────────────────────────────┘
```

A role — a registered participant or a virtual agent — is never reached from
the HTTP side any more (BI-7): `a2a.rs` refuses `{name}` when it names one,
before routing anything. `a2a_participant.rs`'s `submit`/`spawn` are shared
building blocks `BrokerCalls` (the connection's call path, `participant/
calls.rs`) runs a worker's task on, not an HTTP handler.

The gateway holds a single `ModuleRegistry` (behind an `Arc<RwLock<…>>`, read
only to look a module up) whose modules each sit behind their own lock, and a
single `ParticipantRegistry`. Only the MCP handler calls a plugin, through
its `plugin::list-tools` and `plugin::invoke-tool` exports.

## Local participants

A worker registers over a connection the broker made for it and is reached
over that same connection — `invoke_agent`, `list_agents` and the frames
below — never over loopback HTTP (BI-7, ADR-0020): `/a2a/{name}` refuses a
role with 403.

**The connection is the identity** (ADR-0020). The broker admits a node —
`Directory::admit` names it `<spec>-<n>`, never reusing a name — creates a
`socketpair`, keeps one end and hands the other to the child it spawns at
descriptor 3 (`chatty-tui --participant-fd 3`; `open_connection`,
`LocalRunner`). The worker's `session.hello` names nothing: a card's `name`
is ignored, and the hello's result says who the worker is. There is no way
to register otherwise. The shared socket (`with_participant_socket`, `bind`,
`serve`) stays bound and refuses every connection — a hello with an `error`
for its id, anything else by closing it — so no local process can take a
name the broker is about to route a task to.

The connection carries newline-delimited JSON, not A2A: A2A is the gateway's
public wire format, and a child process is not a public endpoint. This is
**version 3**, ADR-0021's one envelope (`participant::FrameCodec`, one per
connection): every line is a request `{"v":3,"id":…,"method":…,"params":…}`,
a result `{"v":3,"id":…,"result":…}`, an error
`{"v":3,"id":…,"error":{"kind":…,…,"message":…}}` or a notification
`{"v":3,"method":…,"params":…}`. There is no fallback to an older version.
What rides in `params`, `result` and `error` is typed in
`chatty_fabric::wire`, split by direction (ADR-0021 § 1, EN-3a): every type
denies unknown fields, nothing is `untagged` or `flatten`ed, and a payload
decodes straight into its struct, so an unknown field, a duplicate key or an
error `kind` outside `WireError` is a decode error that closes the
connection. A task's terminal `metadata` is a `TaskMetadata` (usage, trace,
conversation, handoff keys, evidence); the captured conversation, the
handoff answer and schema and the runner's evidence cross as capped opaque
JSON the broker never reads. `task.run` carries whose task it is as
`identity: {tenant, user}` (absent for a desktop root's task), never a
credential.
(hive's worker moves to v3 with HS-4a, after ADR-0021 step 3.)

```
participant → {"v":3,"id":1,"method":"session.hello","params":{"card":{"name":"",…}}}
broker      → {"v":3,"id":1,"result":{"name":"local-coder-0","scope":"root","owner":null}}
broker      → {"v":3,"id":1,"method":"task.run","params":{"taskId":"task-…","text":"summarise foo.rs"}}
participant → {"v":3,"method":"task.event","params":{"kind":"status","id":1,"state":"working","message":"read_file"}}
participant → {"v":3,"method":"task.event","params":{"kind":"artifact","id":1,"text":"foo.rs defines…","lastChunk":false}}
participant → {"v":3,"id":1,"result":{"state":"completed"}}
```

Each side numbers its own requests and never reuses an id; reusing one still
in flight closes the connection. Results, errors, `req.progress` and
`task.event` name the receiver's request (a task's events name its
`task.run`); `req.cancel` names the sender's — the broker stops a task with
`req.cancel` of its `task.run`. A response or cancel naming nothing in
flight is dropped and logged. Each direction decodes into its own set of
methods, so a method the peer may not send — a worker's `task.run`, say —
closes the connection, as does a line that does not decode at all; an
`error` is only ever sent back for a refused hello.

| Sender | Requests | Notifications |
|---|---|---|
| worker | `session.hello`, `agent.invoke`, `agent.list`, `mailbox.post`, `human.ask`, `human.approve` | `task.event`, `req.cancel` |
| broker | `task.run`, `human.ask` | `req.progress`, `req.cancel` |

**Only the root answers an approval** (ADR-0021 § 2, EN-2a). A worker whose
command or write needs a human sends `human.approve` (`kind: exec | write`,
`command_or_path`, `diff_stat`) and waits for its result, `"approved"` or
`"denied"`. The broker overwrites any `asker` with the connection's admitted
name and chain, and delivers the request straight to the root's call under
an id of its own; no caller in between sees it, and the broker never sends
`human.approve` to a worker. The root's verdict is the request's result. A
worker withdraws its own with `req.cancel`; a closed connection withdraws all
of its own. At most four wait per connection; one more, or one with no root
to ask, is denied.

```
participant → {"v":3,"id":4,"method":"human.approve","params":{"kind":"exec","command_or_path":"[shell] echo hi"}}
broker      → {"v":3,"id":4,"result":"approved"}
```

`scope` is the conversation the node works for and `owner` the node that
asked for it; for now every node is the root's, in scope `root`. `task.run`
also carries the node's `spawnContext` (BI-5, below).

Task states are A2A's (`submitted`, `working`, `input-required`,
`completed`, `failed`, `canceled`); a terminal one is the `task.run`'s
result and ends the task.

**A question is a request that climbs the caller chain** (ADR-0021 § 2,
EN-2b). A worker whose `ask_user` is waiting parks its task on a `human.ask`
request carrying every question with its options, and gets the answers as its
result. The broker overwrites any `asker` with the connection's admitted name
and chain and relays the request, under an id of its own (`question`), to the
caller of the asking worker: a worker gets it as a broker→worker `human.ask`,
the root on its call's stream. A worker answers `escalate`, and the broker
forwards the original request, first stamp intact, to the next caller up, so
the root sees the agent that asked. A question a worker relays from a
third-party A2A peer carries the peer's `origin`, set by the worker's client
code. A worker withdraws its own with `req.cancel`; when the asker's call ends
or is cancelled, the broker withdraws the relayed copy with `req.cancel`.

```
leaf        → {"v":3,"id":2,"method":"human.ask","params":{"questions":[{"id":"q1","question":"Which database?","options":["Postgres","SQLite"]}]}}
broker      → {"v":3,"id":3,"method":"human.ask","params":{"question":"question-1","request":{"questions":[…],"asker":{"agent":"leaf-0","chain":["root","mid","leaf"]}}}}
mid         → {"v":3,"id":3,"result":"escalate"}
broker      → {"v":3,"id":2,"result":[{"id":"q1","answer":"Postgres","custom":false}]}
```

**Calls over the connection (ADR-0020, BI-4).** A worker's `invoke_agent`,
`list_agents` and `send_message` reach local roles and the directory over the
same connection, not over loopback HTTP, as `agent.invoke`, `agent.list` and
`mailbox.post` requests. The broker runs each as the node the connection
names and answers under its id, so several can be in flight and finish in
any order. Closing the connection cancels every call still in flight on it,
which reaps the workers those calls started; a worker may also withdraw one
with `req.cancel`.

```
participant → {"v":3,"id":2,"method":"agent.invoke","params":{"agent":"local-reviewer","prompt":"review it","handle":null,"include_trace":false}}
participant → {"v":3,"id":3,"method":"agent.list"}
broker      → {"v":3,"id":3,"result":[{"name":"local-reviewer","origin":"local",…}]}
broker      → {"v":3,"method":"req.progress","params":{"id":2,"event":{"Step":"read_file"}}}
broker      → {"v":3,"id":2,"result":{"success":true,"response":"Looks good.","metadata":{…}}}
```

A call that cannot run ends with an `error`
(`{"kind":"unknown_agent","agent":…,"message":…}`: the variant's fields beside
`kind`, `message` display text only);
a callee whose task failed ends with a result whose `success` is false.
The in-process root uses `ProtocolGateway::transport()`, the same calls with no
socket.

**The spawn context rides the request** (ADR-0020 invariants 5–6, BI-5). Only
a root process runs a broker; a sub-leader delegates over its connection. A
worker a call starts is spawned with a `SpawnContext {workspace_root,
base_branch, roster, verification, endpoint}` the broker derives from the
calling node's own (its tree, its branch, its roster; the root's verification
command and endpoint for the agent), so a sub-leader's worker branches from the
sub-leader's branch. A call may bring one as `params.spawn_context`; it is
clamped to the caller's own and refused otherwise with an `error`
`{"kind":"spawn_context_refused","field":…,"reason":…,"message":…}`
(`participant::spawn_context`).

At the root, `invoke_agent` asks a question the broker delivers on its own
agent's `ask_user` store, headed by who asked, so the human sees the ordinary
popover. Escalate-to-human is the only policy; whether a leader may answer on a
worker's behalf is an open question on ADR-0011. A root with nobody to ask
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

**Every connection is bounded** (EN-0b, `participant/limits.rs`), and only a
peer's own frame closes its connection. Each end reads lines of at most
`MAX_FRAME_BYTES` (33 MiB, interim until ADR-0021 Q4); a longer line, or one
that does not decode, closes the connection it arrived on. A reply that would
put a frame over the receiver's cap — a callee's result too large for its
caller — fails only that call with an `error`. The outbound queue holds
`OUTBOUND_QUEUE_FRAMES` and makes the producer wait when full, never closing
the slow reader. Calls past `MAX_IN_FLIGHT_CALLS` or `CALLS_PER_SECOND` per
connection are refused. `fuzz/` fuzzes the `FrameCodec` (see its README).

The hosted transport is Firecracker vsock, which reaches this crate as a
plain stream — `serve_connection(stream, registry, admitted_node)` takes any
of them, and `ParticipantConnection::hello_over` is the worker's side of the
same generalization (AGE-307; the hosted broker adopts it with HS-4).

### Virtual agents and the local runner

`ProtocolGateway::with_virtual_agent` publishes one agent that is not a
connected process but a factory. A task addressed to it starts a worker on a
connection the broker made for it, waits for that worker's `session.hello`, routes the
task to it, and reaps it. To its caller — over the caller's own connection,
or the root's direct handle — it is reached exactly like a registered
participant, which is the point: `invoke_agent` replaces `sub_agent` without
the parent learning a second fan-out path.

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
`scan_directory`, and passes it to `ProtocolGateway::new(registry)`.
The desktop (`chatty-gpui`) and `chatty-tui --broker` both do this; see their
module-settings / broker wiring for a worked example, or
`crates/chatty-protocol-gateway/tests/` for a minimal one.

`start()` serves on `gateway.sock` in an owner-only directory
(`access::default_runtime_dir()`: `$XDG_RUNTIME_DIR/chatty-run`, else
`<cache dir>/chatty-run`; `with_runtime_dir` overrides it), and writes the
per-launch token to `gateway.token` (`0600`) beside it. Every route of every
listener serving `build_router()` answers 401 without
`Authorization: Bearer <token>`. It refuses to start when the directory is a
symlink, someone else's, or open to group or others, and it never unlinks a
socket this user does not own. There is no TCP listener. On Windows `start()`
fails with `access::WINDOWS_UNSUPPORTED` (an owner-only DACL is not
implemented yet, tracked by AGE-778): external MCP access is macOS/Linux-only
for now.

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
