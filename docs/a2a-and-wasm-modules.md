# A2A and WASM module architecture

**When to read this:** You need to know how a conversation reaches an agent — a remote
A2A service or a locally installed WASM module — and where the module runtime,
registry and gateway fit.

Authoring a module? Start with
[Build a WASM plugin](../docs-site/src/dev/guides/build-wasm-module.md) (quick start
and host-LLM sequence diagrams); the WIT types are in [wit-reference.md](wit-reference.md).

## Overview

Chatty supports two kinds of agents that can be invoked during a conversation:

| Agent type | Where it runs | How it's called | Configured in |
|:-----------|:--------------|:----------------|:--------------|
| **Remote A2A** | External HTTP service | Direct HTTP to the remote URL | Settings → A2A Agents |
| **Local WASM module** | In-process via Wasmtime | Via the local Protocol Gateway (`localhost:8420` by default) | Settings → Extensions |

Both are **unified behind the same tools** (`list_agents`, `invoke_agent`) and the
same **A2A JSON-RPC protocol**, so the LLM does not need to know which kind it is
talking to.

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

Local modules are never called directly from the tool layer; they are always reached
through the gateway's A2A endpoint. That keeps one code path in `InvokeAgentTool`,
makes local modules speak the same protocol as remote services, and leaves the same
module reachable from other processes on the machine via OpenAI-compatible, MCP and
A2A routes.

## Remote A2A agents

### Configuration

Remote agents are configured in **Settings → A2A Agents** and persisted to
`a2a_agents.json` via `A2aJsonRepository`.

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

Runtime connection status is tracked in `A2aAgentsModel` (a GPUI global) but **not
persisted** — it is refreshed at startup by fetching agent cards.

### Protocol

Remote agents implement the [A2A protocol](https://google.github.io/A2A/). Chatty is
an **A2A client** (`crates/chatty-core/src/services/a2a_client.rs`).

**Agent card discovery** — `GET <base_url>/.well-known/agent.json`, with
`Authorization: Bearer <api_key>` when configured. Chatty reads `name` /
`displayName`, `description`, `skills[].name` (cached into `A2aAgentConfig.skills`)
and `capabilities.streaming`.

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

**Streaming** (`message/stream`): same body with `"method": "message/stream"`. The
response is an SSE (`text/event-stream`) stream where each event carries a JSON-RPC
result:

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

If the server answers with a `Content-Type` other than `text/event-stream`, the client
**falls back** to treating the body as a non-streaming `message/send` response.

## Local WASM module agents

### Architecture stack

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
│   manages load/unload/reload lifecycle)                  │
├──────────────────────────────────────────────────────────┤
│                 chatty-protocol-gateway                   │
│  (HTTP server exposing modules via OpenAI, MCP, and A2A  │
│   protocols simultaneously on localhost:8420)            │
└──────────────────────────────────────────────────────────┘
```

| Crate | Role |
|:------|:-----|
| `chatty-module-sdk` | Guest-side SDK for module authors (types, host import wrappers, `export_module!` macro) |
| `chatty-wasm-runtime` | Wasmtime host: loads `.wasm` components, implements host imports (`llm`, `config`, `logging`, and the optional `file`, `billing`), enforces resource limits |
| `chatty-module-registry` | Discovery (`scan_directory`), lifecycle (`load`/`unload`/`reload`), manifest parsing |
| `chatty-protocol-gateway` | HTTP server (axum) exposing modules via OpenAI, MCP, and A2A protocols |
| `chatty-core` | A2A client (`A2aClient`), agent tools (`list_agents`, `invoke_agent`), settings models and repositories |

### WIT contract

The host–guest interface is
[`wit/chatty-module.wit`](https://github.com/boersmamarcel/chatty2/blob/main/wit/chatty-module.wit)
(package `chatty:module@0.2.0`); [wit-reference.md](wit-reference.md) has the full
type reference.

**Host imports** (what the host provides to the module):

| Interface | Function | Purpose |
|:----------|:---------|:--------|
| `llm` | `complete(model, messages, tools)` | Run an LLM completion via host-managed API keys |
| `config` | `get(key)` | Read key-value config from the module's manifest's `[config]` table |
| `logging` | `log(level, message)` | Emit structured logs; forwarded as A2A progress events by the gateway |
| `file` (optional) | `read-bytes(path)` | Sandboxed read under the manifest's `[files] root`; a module without `[files]` can read nothing |
| `billing` (optional) | `acquire-session(estimated_tokens)`, `report-usage(input, output)` | Paid modules only; reserves and settles credits against a Hive-signed session token |

**Guest exports** (what the module provides to the host):

| Interface | Function | Purpose |
|:----------|:---------|:--------|
| `agent` | `chat(req) → response` | Handle a conversation turn |
| `agent` | `invoke-tool(name, args) → result` | Execute a module-provided tool |
| `agent` | `list-tools() → definitions` | Enumerate available tools |
| `agent` | `get-agent-card() → card` | Return metadata (name, description, skills) |

### Module directory layout

```
<module dir>/
├── echo-agent/
│   ├── module.toml          # Manifest (required)
│   ├── echo_agent.wasm      # WASM component binary
│   └── .chatty-install.json # Install record (only for modules installed from Hive)
└── code-reviewer/
    ├── module.toml
    └── code_reviewer.wasm
```

The directory is configurable via module settings; enabling and disabling an
installed module happens in **Settings → Extensions** (there is no separate
"Modules" settings page). Platform defaults:

| Platform | Path |
|:---------|:-----|
| macOS | `~/Library/Application Support/chatty/modules/` |
| Linux | `~/.local/share/chatty/modules/` (or `$XDG_DATA_HOME/chatty/modules/`) |
| Windows | `%APPDATA%\chatty\modules\` |

### Module manifest (`module.toml`)

```toml
[module]
name = "echo-agent"
version = "0.1.0"
description = "A simple echo agent for testing"
wasm = "echo_agent.wasm"        # Plain relative path inside this directory (no `..`, no absolute)
# execution_mode = "local"      # "local" | "remote" | "remote_only"; remote modules run on the hive-runner

[capabilities]
tools = ["echo", "reverse"]     # Tool names the module exposes
chat = true                     # Implements the chat export
agent = true                    # Acts as an autonomous agent

[protocols]
openai_compat = true            # Expose via /v1/{name}/chat/completions
mcp = true                      # Expose via /mcp/{name}
a2a = true                      # Expose via /a2a/{name} (required for invoke_agent)

[resources]
max_memory_mb = 64              # Memory cap (0 = use default: 256 MiB; may only lower)
max_execution_ms = 30000        # Per-call timeout (0 = use default: 60 s; may only lower)

[config]                        # Optional: string → string values the guest reads via config::get
greeting = "hello"

[files]                         # Optional: the only directory file::read-bytes may read
root = "weights"                # Plain relative path inside this directory
```

Parsing is strict: an unknown key or table, an `execution_mode` other than
`local`/`remote`/`remote_only`, a non-string `[config]` value, or a `wasm`/`[files].root`
path that is absolute or uses `..`, `\` or `:` is a manifest error. A `[resources]` value
above a host ceiling is clamped to it, with a warning on the manifest
(`ModuleManifest::warnings`).

`ModuleRegistry::scan_directory` visits module directories in name order and returns a
`ScanReport { loaded, remote, failed }`: every directory that did not load is in `failed`
with its reason, and remote modules are listed apart from local loads. Two directories
declaring the same `name`: the first by directory name wins, the second is a failure (so is
`load` of a name already registered from another directory). The desktop's installed
extensions list shows a module's failure reason under its row.

**Install hardening (PL-H5a).** `chatty_core::install` checks a registry-supplied module
name against the registry's rule (`^[a-z][a-z0-9-]{1,48}[a-z0-9]$`, no `--`) and the
version as semver before anything touches the filesystem, installs into the configured
`module_dir`, and caps the download at `hive_client::MAX_DOWNLOAD_BYTES` (64 MiB) while it
streams. Each WASM install writes `.chatty-install.json` (`{sha256, trust_level,
publisher_key_id}`) beside the module; the registry hashes the `.wasm` bytes it is about
to compile against that record at every load and refuses a mismatch (`hash mismatch …`,
shown as `Failed to load:`). A module without a record — copied in by hand — loads as
`TrustLevel::Local` (`ModuleRegistry::trust_level`), and Settings → Extensions lists it
under **Local modules**. Signature enforcement (refusing unsigned downloads) is PL-H5,
AGE-608.

A module appears in `list_agents` when `[capabilities].agent = true`, it is
`Loaded` (or `Remote`), and it is enabled in Settings → Extensions
(`collect_module_agents` in chatty-gpui, `discover_module_agents` in
chatty-tui) — `[protocols].a2a` plays no part in that filter. `[protocols].a2a
= true` instead governs whether it can actually be *invoked*: `invoke_agent`
checks the listed module's `supports_a2a` flag (set from `[protocols].a2a`)
and refuses with an error if it is false, even though the module is listed.
Without `a2a = true` the module can still serve tools via MCP or completions
via OpenAI-compat — it just cannot be reached through `invoke_agent`. The
registry skips `execution_mode = "remote"` modules during its WASM scan; the
gateway routes those to the hive-runner instead.

### Resource limits

Every module runs inside a sandboxed Wasmtime instance
(`crates/chatty-wasm-runtime/src/limits.rs`). Every limit is **per call**: fuel is
refilled and the deadline re-armed before each export call. The defaults are the host
ceilings; a manifest's `[resources]` may only lower them — a larger value is clamped
down to the ceiling.

| Limit | Default = ceiling | Enforcement | Error |
|:------|:------------------|:------------|:------|
| **Fuel** | 10¹² units per call | Wasmtime fuel (≈1 unit per Wasm instruction) | `fuel exhausted` |
| **Wall clock** | 60 s per call, host time included | Epoch interruption (10 ms ticks); host imports (`llm::complete`, `file::read-bytes`, billing) stop waiting at the deadline | `deadline exceeded` |
| **Memory** | 256 MiB | Store memory limiter | `memory limit` |
| **Output** | 1 MiB per call | Size of each export's return value | `output too large` |

The fuel ceiling (AGE-708) is sized so a pure CPU-bound guest is bounded by the 60 s
wall-clock ceiling, not by fuel: on this host, Wasmtime fuel runs at roughly
1.5 × 10¹⁰ units/s for a tight arithmetic loop, so 10⁹ (PL-D3's original figure) was
exhausted in well under a second — 10¹² keeps a pure spin running for over a minute of
fuel, past the 60 s wall clock.

`list-tools` and `get-agent-card` get a 1 s wall-clock budget. A guest trap or panic
fails the call with `guest trap: <message>` (the panic message is read from the guest's
stderr) and never takes the host down; the trapped instance is dropped and the module
re-instantiated on its next call, so guest statics start over. Callers can match the
kind with `err.downcast_ref::<chatty_wasm_runtime::CallError>()`.

## Protocol gateway

The gateway (`chatty-protocol-gateway`) is a local HTTP server, bound to
`127.0.0.1` (not `0.0.0.0`) on a port the embedding app chooses — the desktop
defaults to `8420` (`module_settings.json`'s `gateway_port`; there is no UI
field for it yet) — that exposes every loaded module through three protocols
at once:

| Method | Path | Protocol | Description |
|:-------|:-----|:---------|:------------|
| `GET` | `/` | — | JSON index of all modules and endpoints |
| `GET` | `/.well-known/agent.json` | A2A | Aggregated agent card (modules and participants) |
| `GET` | `/a2a/{module}/.well-known/agent.json` | A2A | Per-module agent card |
| `POST` | `/a2a/{module}` | A2A | JSON-RPC: `message/send`, `message/stream`, `tasks/get` |
| `POST` | `/v1/{module}/chat/completions` | OpenAI | Module-specific chat completion |
| `POST` | `/v1/chat/completions` | OpenAI | Model-routed (`model: "module:{name}"`) |
| `POST` | `/mcp/{module}` | MCP | JSON-RPC: `tools/list`, `tools/call`, `initialize` |
| `GET` | `/mcp/{module}/sse` | MCP | HTTP+SSE transport: the event stream |
| `POST` | `/mcp/{module}/sse?sessionId=…` | MCP | HTTP+SSE transport: a client message (answered on the stream) |

A module is served only on the protocols its `[protocols]` enables (the others
answer 404). Every route refuses a non-loopback `Host` or `Origin` with 403 (DNS
rebinding) and a body over 10 MiB with 413. Each module has its own lock, so a slow
call to one module never holds up another; a guest reply over the 1 MiB output cap
is a 502. The OpenAI routes keep every message's role, pass the request's `user` as
`conversation_id`, and refuse `stream: true` with 400. The per-route details are in
[`crates/chatty-protocol-gateway/README.md`](../crates/chatty-protocol-gateway/README.md#what-every-route-enforces).

### A2A via the gateway

For a local module, `invoke_agent` constructs an `A2aAgentConfig` whose `url` is
`http://localhost:<port>/a2a/{module}` (no API key, `skills` = the module's tools) and
calls the same `A2aClient::send_message_stream()` used for remote agents.

The gateway's `message/stream` handler (`handlers/a2a.rs`):

1. Emits `{"status": {"state": "working"}, "final": false}` immediately
2. Runs the module's `chat()` in a task of its own (the guest itself on the blocking
   pool) with every text part of the message, after the earlier turns of the same
   `contextId`; a caller that disconnects does not cancel it
3. Forwards the module's `logging::log()` calls as `working` status events through an
   `mpsc` channel, so `log::info("Processing step 3…")` shows up live in the UI
4. On completion, emits the artifact (`parts[0].text`) and a final
   `{"status": {"state": "completed"}, "final": true}`
5. On error, emits `{"status": {"state": "failed"}, "final": true}`

`GET /.well-known/agent.json` returns a gateway-level card
(`{"schema_version": "0.1", "gateway": true, "agents": [...]}`) listing every loaded
module agent and every registered participant with its name, `displayName`,
description, version, skills and `capabilities.streaming`.

### Local participants (ADR-0011)

`{module}` in the A2A routes above also resolves a **local participant**: a
worker process the broker spawned on a connection it made for it, which
published an agent card and answers tasks over that connection. ADR-0011
routes all fleet coordination — local and hosted — through this one broker
rather than through a second fan-out path, so a child process and a WASM
module are the same thing to an A2A caller. Participants are looked up
**first**, so a live process would shadow a module of the same name.

**The connection is the identity** (ADR-0020, AGE-635). The broker admits a
node, which names it `<spec>-<n>` (`local-coder-0`), creates a `socketpair`
and hands one end to the child at descriptor 3 (`--participant-fd 3`); the
child marks it close-on-exec as `main`'s first statement, so no shell or tool
it starts inherits it. The child's `hello` names nothing — its card's `name`
is ignored — and the broker's `welcome` tells it its name, scope and owner.
Nothing registers any other way: the shared participant socket stays bound
and answers every connection with an `error` frame, so no local process can
take a name the broker is about to route a task to.

The connection carries newline-delimited JSON frames, protocol **v2**: every
frame carries `"v":2` (`hello`, `welcome`, `error`, `task`, `status`,
`artifact`, `cancel`, `input`, and the call frames below), and a frame without
it is answered with an `error` frame naming v2 and the connection is closed —
there is no v1. The gateway maps the task frames onto the same A2A status and
artifact updates a module produces.

**Workers call over the same connection (ADR-0020, BI-4, AGE-636).** A
worker's `invoke_agent` and `list_agents` reach local roles and the broker's
directory as `call` frames on its own connection, never over loopback HTTP:

| Direction | Frame | Fields |
|---|---|---|
| worker → broker | `call` | `id` (the worker's, unique on the connection), `method` (`invoke_agent`, `list_agents`, `send_message`), `params` |
| broker → worker | `call_progress` | `id`, `event` — an `InvokeAgentProgress` as JSON: `{"Step": "read_file"}` for a line about the callee's work, `{"Text": "…"}` for its answer as it streams |
| broker → worker | `call_result` | `id`, `result` — for `invoke_agent` `{success, response, error?, metadata?}`, the callee's terminal status as an A2A caller reads it (usage, trace, conversation and evidence ride in `metadata`); for `list_agents` the aggregated card's `agents` array |
| broker → worker | `call_error` | `id`, `error: {kind, message}` — the call could not run (`unknown_agent`, `refused`, `spawn_context_refused`, …) |
| broker → worker | `call_input_required` | `id`, `task` (the callee's parked task), `request` — the question, `{id, questions}` as a parked task's `input` carries it (BI-5) |
| worker → broker | `call_input` | `id`, `task`, `input` — the answer, `{requestId, answers}`, the same shape as an `input` frame (BI-5) |

```text
worker → {"v":2,"type":"call","id":1,"method":"invoke_agent","params":{"agent":"local-reviewer","prompt":"review it","handle":null,"include_trace":false}}
broker → {"v":2,"type":"call_progress","id":1,"event":{"Step":"read_file"}}
broker → {"v":2,"type":"call_result","id":1,"result":{"success":true,"response":"Looks good.","metadata":{…}}}
```

The call says nothing about its caller: the broker runs it as the node the
connection names (the permit's AGE-628 check, the edge log's `from`), and
writes one edge-log row per `invoke_agent` call when it ends
(`<data_dir>/chatty/fabric/edges-<pid>.jsonl`; `list_agents` is a directory
read, not an edge, and writes none). Several calls can be in flight in one
task; replies match by `id`, in whatever order the calls finish. A callee
whose task failed is a `call_result` with `success: false`, not a
`call_error`, so the caller renders it exactly as a failed A2A task. When a
worker's connection closes, the broker cancels every call still in flight on
it, which reaps the workers those calls started — so cancelling a leader's
task reaps its whole subtree (invariant 11).

**A question comes back down the call (BI-5, AGE-637).** When a callee parks
its task on `ask_user`, the broker sends the calling worker
`call_input_required` with the call's `id` and the parked task; the worker's
`invoke_agent` re-asks it on the worker's own clarification store — which
parks the worker's own task toward *its* caller — and sends the answer up as
`call_input`. The broker delivers it only to the task that call parked, so a
worker answers its own callees and nobody else's. A grandchild's question
therefore reaches the root's human however many workers sit between them, and
the answer descends the same hops
(`clarification_relays_across_two_hops`):

```text
broker → {"v":2,"type":"call_input_required","id":1,"task":"task-…","request":{"id":"req-…","questions":[{"id":"q1","question":"Which database?","options":[]}]}}
worker → {"v":2,"type":"call_input","id":1,"task":"task-…","input":{"requestId":"req-…","answers":[{"id":"q1","answer":"SQLite","custom":false}]}}
```

**Spawn context: sub-leaders use the root broker (ADR-0020 invariants 5–6,
BI-5, AGE-637).** Only a root process starts a broker. A sub-leader — a worker
whose spec delegates in turn — is spawned with no `--broker`, and
`--broker` inside a worker (`--participant-fd`) starts nothing: its calls go
over its connection like any worker's. What its own broker used to decide
travels on the spawn request as a `SpawnContext {workspace_root, base_branch,
roster, verification, endpoint}` (on the `invoke_agent` call's `params` as
`spawn_context`, and on the child's `task` frame). The broker sets it from the
calling node's own context: the node's own worktree and branch and the roster
it was given, with the verification command and endpoint the root's settings
give the agent being spawned. So a sub-leader's worker gets its tree under the
sub-leader's tree, on a branch that starts at the sub-leader's branch, and its
evidence diffs against that branch (`grandchild_branches_from_its_subleader`).
A call that brings a context of its own is clamped: accepted only if
`workspace_root` lies inside the caller's own tree, `base_branch` is the
caller's, `roster` is a subset of the caller's, `verification` is the root's
(or none) and `endpoint` is the root's for that agent. Anything else ends the
call with `call_error` `spawn_context_refused`, whose `message` is
`{field, reason}` naming the field (`spawn_context_is_clamped`), so a
`team.json` a model wrote into its worktree cannot loosen anything. A call to
a virtual agent outside the caller's roster is `refused`. Usage folds up both
hops through the root (`usage_folds_across_two_hops`).

A worker **connects before it builds its agent**: `chatty-tui
--participant-fd` says `hello`, gets `welcome`, and only then builds the agent
with the connection's transport (`WorkerConnection::transport`,
`AgentBuildContext::fabric_transport`), so its tools hold the connection from
the first turn. The in-process chatty-tui root reaches its own broker the
same way minus the socket: `LazyBroker::transport` hands `invoke_agent` and
`list_agents` a `DirectTransport` into the broker. Remote agents and WASM
modules stay on `A2aClient`. The gateway counts HTTP requests for roles and
the directory (`RouteCounter`); in a swarm of workers both stay at zero
(invariant 4, `no_worker_call_uses_loopback`). The connection is the liveness
signal: closing it deregisters the participant and fails every task it still
owed. A worker's `ask_user` parks its task in `input-required` with the
question attached; the caller answers with `message/send` on the same task id
and the broker hands the answer down as an `input` frame (AGE-306). The frames
and the mapping are documented in
[`crates/chatty-protocol-gateway/README.md`](../crates/chatty-protocol-gateway/README.md#local-participants).

The participant path is Unix-only. The hosted transport is Firecracker vsock,
which arrives here as an ordinary stream: both `serve_connection` and
`ParticipantConnection` take any `AsyncRead + AsyncWrite`, so the frames, the
hello/welcome and the liveness rule are shared rather than reimplemented
(AGE-307, in `boersmamarcel/hive`; hive speaks v1 at its current chatty2 pin and
adopts v2 with HS-4, AGE-678).

## LLM-facing tools

`list_agents` and `invoke_agent` are registered by `AgentFactory` in **every**
conversation.

### `list_agents`

One flat list of everything addressable, each entry saying whose machine it runs on:

```json
{
  "agents": [
    { "name": "echo-agent", "origin": "local", "kind": "module", "description": "...", "enabled": true, "skills": ["echo"] },
    { "name": "leased-vm", "origin": "fleet", "kind": "worker", "description": "...", "enabled": true },
    { "name": "local-agent", "origin": "local", "kind": "worker", "description": "...", "enabled": true },
    { "name": "voucher-agent", "origin": "remote_configured", "kind": "remote", "url": "https://...", "enabled": true, "has_api_key": true, "skills": ["..."] }
  ],
  "total": 4,
  "note": "To invoke an agent, use the `invoke_agent` tool..."
}
```

Two sources feed it. **Settings** give the configured remotes and the installed
modules. The **broker's aggregated card** gives whatever registered since — a worker
spawned a minute ago is addressable, and only the broker knows it exists. A name in both
keeps the settings label, because what the user configured is the more informative
answer. A gateway that is off or slow to answer is not an error: the list is then what
settings know. API key values are **never exposed** to the LLM — only
`has_api_key: true/false`.

#### `origin` — whose machine it runs on (ADR-0011 C5)

| Origin | Means | Inside the fleet? |
|:-------|:------|:------------------|
| `local` | a process on this machine: a spawned worker, a WASM module | yes |
| `fleet` | elsewhere in this user's fleet — a leased microVM registering over vsock | yes |
| `remote_configured` | a URL from Settings → A2A Agents: a third party, chosen deliberately | no |
| `discovered` | learned from another agent's card rather than configured | no |

The label is a property of the **registration**, not of the card: a participant
describes itself, and the broker says where it came from, or the label would be worth
nothing. `AgentOrigin` is defined once, in `chatty-fabric`:
`chatty-protocol-gateway` serves it and `chatty-core` reads it, and both depend on
that crate.

Nothing publishes `discovered` yet: chatty has no peer-discovery hop. The label exists
so that one cannot be added without deciding what it means.

### `invoke_agent`

```json
{ "agent": "echo-agent", "prompt": "Hello, agent!" }
```

Resolution order: remote A2A agents first (a remote agent shadows a local module with
the same name), then **`local-agent`** — the broker's local worker (below) — then local
module agents, which require `supports_a2a = true` and a running gateway (otherwise the
tool reports that the gateway is off and points to Settings → Modules). Every path
streams through `A2aClient::send_message_stream()`; progress (`InvokeAgentProgress`) is
forwarded to the UI so the user sees intermediate output while the tool call is in
flight.

**Before a prompt leaves the fleet.** With `warn_on_external_agent` on (execution
settings, off by default), `invoke_agent` says on the progress channel that the prompt
and anything quoted in it are about to go to an agent whose origin is not `local` or
`fleet`, naming the agent, the URL and the origin. It only says so: whether an external
agent should need an allowlist, a one-time confirmation, or nothing at all is a product
decision that has not been made, and this is the hook it will hang from.

**`include_trace`** (AGE-467, default `false`): a leader that delegates the same task to
several workers and wants to judge *how* each one got its answer, not just what it
concluded, sets `include_trace: true` on the call. `InvokeAgentOutput.trace` is then the
worker's compacted tool-call trace — otherwise the field is absent from the JSON the
model sees, so an ordinary delegation costs no more context than it did before this
existed.

### `local-agent` — a chatty agent in its own process

`invoke_agent { "agent": "local-agent", "prompt": "…" }` asks the broker for a worker.
The gateway spawns `chatty-tui --participant-fd 3` on a connection it made, the child
says hello, and its turn comes back as A2A status and artifact updates. This is
One fan-out path, a public wire format, and a place to put discovery, budgets and the
ledger.

The child maps its `SessionEvent`s to frames with
`chatty_protocol_gateway::worker::TaskMapper` (the `worker` feature) — tool starts and
finishes become `working` status messages (and so do the steps of anything the child
itself delegated, `InvokeAgentProgress::Step`, so a grandchild's tool calls reach the
leader one line each), assistant text becomes artifact chunks, and
the turn's token usage rides in the terminal status's `metadata` under `usage` (A2A has
no usage concept; usage belongs to the ledger). It goes as `lines`, one per model, each
naming its model and carrying tokens and time but no price (AGE-682). The lines already
include whatever the child itself delegated (merged only with lines on the same model),
and the parent's `invoke_agent` folds them into its own conversation as usage lines
marked `delegated_to`, priced in `finish_turn` at the model each names — so a leader's
`total_cost` carries the whole tree below it, and the bill follows the bearer (AGE-415). The same terminal status carries a
second, independent key, `trace` (AGE-467): the worker's compacted tool-call trace —
one `### <tool> (ok|FAILED|no result)` block per call it made, with the call's input and
output or error, capped and (past 40 calls or 12 000 characters) trimmed from the middle
— present only when the mapper saw at least one tool call. It rides unconditionally;
whether it reaches the model is `invoke_agent`'s call, gated on `include_trace` (below),
so a plain delegation's wire cost is unchanged. The mapper and the one-task loop around
it live beside the broker's own half of the protocol, not in this crate, because a
microVM's `chatty-server` is a worker too and the parent must not be able to tell the
two apart.
`crates/chatty-tui/src/participant/equivalence.rs` asserts the
parent's tool-call trace carries every tool call the child reported, for every
scripted scenario — how ADR-0011's first kill criterion is checked in CI rather than
by inspection.

A worker's `ask_user` does not end at the worker. `invoke_agent` re-asks the
question on its own agent's clarification store: with a human behind it that is
the ordinary `ask_user` popover, and in a worker it parks that worker's own task
in `input-required` toward *its* caller, so a question climbs the chain until it
reaches someone who can answer and the answer descends the same hops
(ADR-0011 C7). `crates/chatty-tui/src/participant/input_required_chain.rs`
runs a parent → child → grandchild chain over a real socket and asserts the
grandchild's question reaches the parent's popover and its answer comes back.

That chain is carried by `message/stream`. A caller that started the task with
plain `message/send` has a single reply object with no room for a non-terminal
update, so a worker parking under it is asking someone who cannot hear. The
broker ends such a task immediately and quotes the question in the failure,
rather than letting it wait out the worker's clarification timeout (AGE-321) —
the caller learns what was wanted and can ask again over `message/stream`.
Holding the task open for `tasks/get` polling would make non-streaming callers
first-class and is the A2A-shaped answer; it was weighed and not taken, because
it makes the broker stateful for open tasks and every delegation path here
streams.

Each worker runs in its own `git worktree` under the conversation's workspace
(ADR-0012), through `chatty_core::services::worker_tree`: `.chatty/worktrees/<name>`
on branch `sub-agent/<name>`, where `<name>` is the participant name (`local-coder-0`)
unless that branch or directory already exists in the repository — another root
process on the same repository counts its own workers from zero, and a tree left from
an earlier run keeps its branch — in which case it is the first free `<name>-N`
(AGE-402). A sub-leader's worker gets its tree under the sub-leader's, branched from
the sub-leader's branch, and its evidence is measured against that branch (BI-5). The evidence envelope
appended to the worker's answer names the branch actually created. In a repository, a
tree that cannot be made fails the delegation; only a workspace that is not a
repository runs its workers unisolated.

**Per-endpoint concurrency budget (ADR-0011 C6).** Workers all talk to the same model
server, so the broker holds a semaphore per *endpoint* — the server's base URL, not a
model and not a worker — and a task waits for a slot before a child is spawned. On a
local Ollama, three concurrent workers on one loaded model is not three times the
throughput; it is the fourth request evicting the weights the first three are using.
The slot is held from just before the spawn until the worker is reaped, so the same
event that frees the process and its worktree admits the next queued task.

The size, in order: an explicit override in `endpoint_budgets` in `module_settings.json`
(keyed by base URL, e.g. `http://localhost:11434`; no UI yet), then what the provider
reports about itself (`num_parallel` in its `extra_config`, or a local Ollama's `OLLAMA_NUM_PARALLEL`
from the environment), then `default_endpoint_budget`, which is **1**. Every wait is a
`tracing` event carrying the endpoint, its limit and the queue depth at that moment, so
a budget that is too tight looks like a queue that never empties.

> A cloud endpoint gets the same default of 1 unless it is overridden. It is the knob
> to turn first if delegation feels serialised on OpenRouter.

**Headless leaders need `--broker` (AGE-376).** The wiring above is started by
chatty-gpui's module-settings controller, so only the desktop had a `local-agent` to
delegate to — a `chatty-tui --headless` or `--pipe` leader (and the harness-driven
chatty behind a benchmark, e.g. the Harbor adapter, AGE-285) had none. `chatty-tui
--broker` runs the same gateway and socket in-process: an ephemeral HTTP port, a
participant socket suffixed with this leader's pid (so two headless leaders on one
host never collide), and a `local-agent` `LocalRunner` sized against the same
per-endpoint budget as the desktop. It carries no WASM module registry of its own —
`--broker` exists to make `local-agent` reachable, not to load modules — and it is
valid with `--headless`, `--pipe` and the interactive TUI alike. Workspace isolation is
the same `git worktree`-per-worker as the desktop when `--workspace` (or the persisted
workspace) is a git repository. When the turn (or the session) ends the gateway stops
serving; any worker still running is reaped by the runner the same way it always is.
`--broker` never writes module settings: its ephemeral port lives only in this run's
agent-build context, so `/modules` in the same session still saves exactly what was on
disk (AGE-382).

**The broker starts lazily, on first use (BI-2, AGE-634).** Neither host starts the
broker at boot: `--broker`/`--team` (chatty-tui) and the module gateway setting (the
desktop) only prepare it — a `chatty_core::services::lazy_broker::LazyBroker` — and the
first `list_agents` or `invoke_agent` call is what actually binds its socket and TCP
port, through `LazyBroker::ensure_started`. Later calls reuse the same broker
(memoized). A test can read `LazyBroker::bound_addrs()` to see whether anything is
bound yet without triggering a start.

**Named virtual agents (ADR-0011 C10, AGE-377; agent specs, AGE-614).** Every worker
used to be the same: the roster's default model with the leader's tools. A team is now a
list of *agent specs* (below), each published by the broker under its own name with its
own model and tool set. `module_settings.json` names them:

```json
{ "virtual_agents": ["local-coder", "local-reviewer"] }
```

A file that still lists agent objects here (the old `VirtualAgentConfig` shape) fails to
load with an error naming `virtual_agents`; there is no compatibility shim.

**Agent specs (PL-D2, AGE-614).** An agent spec is the one declarative definition of an
agent, the same for a worker, a `--team` leader and `chatty-tui --agent <name>`. TOML on
disk, the same shape as JSON on the wire:

```toml
[agent]
name = "local-reviewer"            # the A2A address, /a2a/{name}
description = "Verifies a worker's branch"
model = "gemma4:26b"               # resolved like --model; absent = the roster's default
preamble = "Verify, do not trust, verdict first. …"

[tools]
profile = "reviewer"               # coordinator | coder | reviewer
disable = ["fetch"]                # tool groups, narrows only
skills = ["coder-reviewer"]        # read_skill names, named in the preamble

[[plugins]]                        # its tools become this agent's (PL-U2)
module = "benford-agent"
version = "^0.2"                   # checked against the module's [module].version
grants = ["llm"]                   # "http" / "file-write" would make every call ask first
limits = { max_execution_ms = 5000 }  # lowers the module's [resources], never raises them

[swarm]
delegates_to = ["local-coder"]     # whom it may call (names or * globs); empty = no agent tools
exposed = true                     # others may call it (the default)
callers = ["coder-reviewer-leader"]  # optional: only these may call it

[budget]
max_agent_turns = 30               # 0 = uncapped; the deadline applies
max_duration = "30m"
cap_usd = 2.0                      # per task, the spend gate's cap
```

A spec is looked up in `<workspace>/.chatty/agents/<name>.toml`, then
`<data_dir>/chatty/agents/<name>.toml` (Linux: `~/.local/share/chatty/agents/`), then the
presets compiled into `crates/chatty-core/agents/`; the first match wins and shadows the
rest (`chatty_core::agent_spec::{load_agent_spec, list_agent_specs}`). Unknown fields are
an error naming the field (`extra_args` is gone), and validation reports every problem at
once: bad name, unknown profile, unknown tool group, bad model reference, duplicate
plugin, bad duration or cap. `AgentBuildContext::from_spec` is the one place a spec
becomes what an agent is built with: the role, the execution settings narrowed by
`disable` and `max_agent_turns` and then gated, the skills line, and a per-task
`TaskSpendGate` from `cap_usd`. A worker receives its whole spec on its argv as
`--agent-json <spec>` and builds itself through that same function.

| Field | Meaning |
|-------|---------|
| `agent.name` | The name `invoke_agent` addresses, served at `/a2a/{name}`; lowercase letters, digits, `-`, `_`, and the file's own name. |
| `agent.model` | Optional. Resolved as `chatty-tui --model` resolves it (id, then name, then a substring of the model identifier). Absent: the roster's default. |
| `agent.preamble` | Optional. The role's standing instructions, appended to the system prompt after the base preamble, before the tool summary. |
| `tools.profile` | Optional. A named tool profile: `coordinator`, `coder` or `reviewer` — see below. |
| `tools.disable` | Optional. Tool groups switched off: `shell`, `fs-read`, `fs-write`, `fetch`, `git`, `code-exec`, `docker-exec`, `ask-user`, `terminal` (`_` works for `-`). Composes with the profile: it can narrow it further, never re-enable a tool the profile excludes. |
| `tools.skills` | Optional. Skills the role is told to `read_skill` before it starts. |
| `swarm.delegates_to` | Optional. The agents it may call, by name or `*` glob (`"*-reviewer"`). Non-empty is what gives an agent `list_agents` and `invoke_agent`, whatever its profile; empty or absent, it has neither (PL-S2 DP-1). |
| `swarm.exposed` | Optional, default `true`. `false`: no other agent may call it. |
| `swarm.callers` | Optional. When set, only these agents (names or `*` globs) may call it. `chatty_core::services::delegation_policy::may_call` checks both specs: the caller's `delegates_to`, then the callee's `exposed` and `callers`. |
| `budget.max_agent_turns` | Optional. The agent's own turn budget (AGE-440). Absent: an unattended run has no turn cap and a 30-minute time budget. |
| `budget.max_duration` | Optional. The wall-clock budget, as `--max-duration` writes it. |
| `budget.cap_usd` | Optional. Dollars one task may spend before `invoke_agent` refuses to start another delegation. |
| `plugins` | Optional. WASM modules whose tools this agent runs in-process — see below. |

**Plugins: a module's tools as the agent's own (PL-U2, AGE-616).** Each `[[plugins]]`
entry is loaded when the agent is built: the module directory (`module_settings.module_dir`)
is searched for a `module.toml` whose `[module].name` is `module`, its version is checked
against `version`, and one instance is made for this agent, with the module's `[config]`
(the spec's `config` on top) and `[files].root`, and its `[resources]` lowered by the
spec's `limits`. Every tool its `list-tools` names is registered next to the native tools
as `<module>__<tool>` — `echo-agent__reverse` — because OpenAI-wire providers (OpenRouter,
Azure) refuse any tool name outside `^[a-zA-Z0-9_-]{1,64}$`, so the dotted form is only
what the transcript shows ("Ran echo-agent.reverse"). A call goes straight into the
instance under PL-H1's per-call limits — about 40 µs, against about 1 ms through the
gateway's `/mcp/{module}` — and a trap, deadline or guest error comes back to the model as
the tool's error, with its reason, and the turn goes on. The spec is the plugin's
allow-list: a tool profile does not remove it. A call asks for approval only when the
spec grants the plugin a side-effecting capability (`http`, `file-write`). Under
`--tool-loading dynamic` each plugin is one `load_tools` group named after it. What a
plugin spends through `llm::complete` runs on the calling agent's model and is recorded
as its own usage line on the turn, naming the plugin and the model that served it. A
plugin that does not load fails the agent's build. The desktop runs spec agents as
`chatty-tui` workers, which load their plugins the same way; it no longer adds its modules
to the MCP server list — `/mcp/{module}` is for MCP clients outside chatty.

**Roles: a profile and a preamble (ADR-0011 C11, AGE-405).** `tools.disable` removes
whole tool *groups*, which is the wrong grain for a role — a reviewer wants `git_diff`
but not `git_commit`. `tools.profile` names a profile
instead: an allowlist of tool *names*, and the worker's whole tool set. Anything the
profile does not name is dropped, MCP tools included, which is most of the point — a 4B
coder used to be handed 53 tool schemas (~13k tokens) before it could read a file. A
profile only ever removes tools: it cannot turn on a group the execution settings
switched off. A profile does not decide delegation: `list_agents` and `invoke_agent`
come with a non-empty `swarm.delegates_to`, on any profile or none, and no profile removes them.

| Profile | What it can call |
|---------|------------------|
| `coordinator` | The read set below, plus the todo plan (`write_todos`, `update_todo`, `verify_completion`) and `git_merge` (AGE-404: how a leader without a shell takes a worker's branch; on a conflict the tool lists the conflicting files and leaves the tree for the leader to report). It does not edit; the `coder-reviewer-leader` preset delegates because its spec lists `local-coder` and `local-reviewer`. |
| `coder` | The read set, plus the filesystem-write tools, the shell, the writing half of git (`git_add`, `git_create_branch`, `git_switch_branch`, `git_commit`, `git_merge`), `execute_code`, the data-query tools (`query_data`, `describe_data`, `profile_data`, `file_structure_detector`) and the memory tools (`remember`, `save_skill`, `search_memory`; AGE-456). |
| `reviewer` | The read set, plus the shell so it can run the tests and the data-query tools (`query_data`, `describe_data`, `profile_data`, `file_structure_detector`) so it can independently re-derive a claimed data-derived value. No writes, no commits. |

The read set every profile starts from is `read_file`, `list_directory`, `glob_search`,
`search_code`, `git_status`, `git_log`, `git_diff` (which takes a `base..head` `range`, so
a reviewer reads `main..sub-agent/<name>` without a shell), `read_skill`, and `ask_user` —
every profile keeps that last one, or a worker could no longer park a question on its leader
(AGE-306). The todo plan is the leader's and the unprofiled main agent's: a worker gets one
bounded task and does not plan it again (AGE-479). The
profiles live in `chatty_core::factories::tool_profile`; a spec (or `chatty-tui --tools`)
naming an unknown profile is refused rather than starting a worker with every tool there is.

`preamble` is the other half of a role: without it a reviewer only knows it is a reviewer
if the leader says so in the task, which is exactly how a reviewer came to approve a
one-line branch on the coder's word. It lands in the worker's system prompt ahead of the
tool summary, and its first sentence goes on the agent's card so the leader can pick by
reading.

An empty or absent list is the single `local-agent` of before. A role is a spec and
nothing else: `invoke_agent` takes no `model` or `role` parameter, so the
leader's tool schema and prompt prefix are identical whatever the team, and each
agent's card — what `list_agents` shows — says which model it runs, which tool profile or
tool groups it has, and the first sentence of its preamble, so the leader chooses by
reading rather than guessing. Each agent is metered
on the endpoint of *its* model (`chatty_core::services::worker_endpoint`), so a
reviewer on another server does not queue behind the coder, while two agents on one
server share that server's budget. Both frontends build their runners from
`chatty_core::services::virtual_agents::resolve_virtual_agents`; a `--broker` leader
started with `--ollama`, `--openai-compat-url` or `--api-key` forwards those flags to
every child (a Harbor sandbox has no `providers.json` for a child to read), and a
settings-configured desktop leader forwards nothing. There is no settings page for this
yet; the JSON is the interface.

**The evidence envelope (ADR-0011 C12, AGE-406).** A worker's report is the worker's
account of what it did; the *runner* can say what it actually left behind, and it does.
When a delegated task ends the runner commits the worker's worktree and, before the
caller sees a terminal event, reads the tree back: the branch, how many commits it
carries over the default branch (`main` or `master`, whichever exists), `git diff --stat
<default>..<branch>`, and — when the team declares a verification command — that
command's exit code and last 20 lines, run with the shell in the worktree under a
five-minute timeout. That envelope is appended to the worker's answer as a fenced
`evidence` block and carried, structured, on the terminal status's
`metadata.evidence`, so a trace or Harbor's ATIF reads it without parsing prose. It
replaces the branch hint of AGE-399. A worker that committed nothing gets **no
envelope at all** — a read-only reviewer must never be handed something to merge — and
nothing is ever merged automatically; merge policy belongs to the flow.

The verification command is the team's, not an agent's, and lives next to
`virtual_agents` in `module_settings.json`:

```json
{
  "team": { "verification": "python3 -m unittest discover -s tests -t . -v" }
}
```

| Field | Meaning |
|-------|---------|
| `team.verification` | Optional. A shell command the runner runs in each worker's worktree once its task ends, whose exit code and output tail go into the evidence envelope. Absent: the envelope carries branch, commits and diff stat only. |

It is skipped for any agent **whose profile has no shell**: a worker that could not run
commands produced no build, so running the suite in its tree would report the leader's
own state back as the worker's. `tools.profile` and `tools.disable` compose (AGE-452), so
both have to allow it: a `reviewer` runs the suite unless `disable` also names `shell`,
a `coordinator` never does regardless, and with no profile named, `disable` containing
`shell` alone is what skips it. The command is a plain subprocess in a
process group of its own, not one of the agent's tools — the point of the envelope is
that it is the runner's fact and not the worker's account of one — and the timeout
kills that whole group, so a suite that hangs cannot outlive the delegation that
started it.

### Teams

**The team directory (ADR-0011 C13, AGE-407).** Everything above that makes a working
team — the roster in `module_settings.json`, its `team.verification`, the leader's
role, a skill the leader follows and a turn budget in `execution_settings.json` — used
to live in four files and a shell script. A *team directory* is that in one place a
Harbor arm can upload and a run can reproduce: `teams/<id>/team.json` with `SKILL.md`
beside it.

`team.json` is a thin file of agent spec names (AGE-614):

```json
{
  "leader": "coder-reviewer-leader",
  "agents": ["local-coder", "local-reviewer"],
  "verification": "cargo test --all-features -- --test-threads=1",
  "skill": "coder-reviewer",
  "max_agent_turns": 50
}
```

| Field | Meaning |
|-------|---------|
| `leader` | The spec the leader runs as. `--model`, `--tools` and `--preamble` still beat its fields. |
| `agents` | The roster, by spec name. Replaces `module_settings.virtual_agents` for the run. |
| `verification` | Optional. The team's verification command (`team.verification` above) for the run. |
| `skill` | Optional. The skill the leader is told to follow: its first turn opens with `read_skill <skill> and follow it`, plus the verification command when one is declared, since a `coordinator` leader has no shell and can only delegate the check. `read_skill` serves the `SKILL.md` beside `team.json` ahead of the skill directories. |
| `max_agent_turns` | Optional. The leader's turn budget for the run, ahead of the leader spec's own; without either a headless leader has no turn cap and a 30-minute time budget. A worker's budget is its own spec's. |

A `team.json` in the old shape — a `leader` object, agent objects in `agents` — fails to
load with an error naming the field.

`chatty-tui --team <id>` runs as that team's leader. It implies `--broker`, declares the
roster from the team file (nothing is written back to `module_settings.json`), runs as
the leader's spec (an explicit `--model`/`--tools`/`--preamble` beats it), sets
the turn budget, and opens the first turn with the skill instruction. Valid with
`--headless`, `--pipe` and the interactive TUI. The id is looked up in
`<workspace>/.chatty/teams/<id>/`, then `<data_dir>/chatty/teams/<id>/` (Linux:
`~/.local/share/chatty/teams/`), then the presets compiled into the binary; the first
directory with a `team.json` wins, and a malformed file there is an error rather than a
fall-through to the preset. The specs it names are looked up the same way
as any spec, so a workspace spec shadows the preset one. The loader is
`chatty_core::services::team::load_team`; the presets are
`crates/chatty-core/teams/` and `crates/chatty-core/agents/`.

One preset ships, `coder-reviewer`: a `coordinator` leader, `local-coder` on the `coder`
profile, `local-reviewer` on the `reviewer` profile with the "verify, do not trust,
verdict first" preamble, the `coder-reviewer` skill beside it (the reviewer finds the
default branch with `git_status` and reads `<default>..<branch>` with `git_diff`'s
`range`; the leader merges with `git_merge` on APPROVE and then has the reviewer run the
team's verification command on the merged tree, since a coordinator has no shell), and
a 50-turn budget. It names no models: they come from the roster's default, `--model`, or
a `team.json` of your own that overrides it.

```bash
chatty-tui --team coder-reviewer --headless --ollama --model qwen3:14b \
  -m "Fix the overdraft bug in src/account.py; the acceptance criterion is that tests/test_account.py passes."
```

**Ollama thinking models as leaders (AGE-400).** A thinking model such as `qwen3`
sometimes writes its tool call inside the thinking channel; Ollama surfaces tool calls
only from content, so the call is lost and the model's answer arrives empty. On a leader
or reviewer that shows up as a delegation chain that stops without a word. The roster
entry's `extra_params.think` is Ollama's per-request `think` switch (`"true"` or
`"false"`, sent as the request's top-level `think` field); set it to `"false"` on any
Ollama model that coordinates or reviews, in `models.json`:

```json
{ "id": "qwen3-14b", "provider_type": "ollama", "model_identifier": "qwen3:14b",
  "extra_params": { "think": "false" } }
```

Absent, Ollama's model default applies. Other providers ignore the key. The desktop's
model dialog has no field for it and rewrites `extra_params` when a model is saved
there, so a model edited in the dialog needs the key put back by hand.
