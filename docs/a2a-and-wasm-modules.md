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
| **Local WASM module** | In-process via Wasmtime | Via the local Protocol Gateway (`localhost:8420` by default) | Settings → Modules |

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
│   manages load/unload/hot-reload lifecycle)              │
├──────────────────────────────────────────────────────────┤
│                 chatty-protocol-gateway                   │
│  (HTTP server exposing modules via OpenAI, MCP, and A2A  │
│   protocols simultaneously on localhost:8420)            │
└──────────────────────────────────────────────────────────┘
```

| Crate | Role |
|:------|:-----|
| `chatty-module-sdk` | Guest-side SDK for module authors (types, host import wrappers, `export_module!` macro) |
| `chatty-wasm-runtime` | Wasmtime host: loads `.wasm` components, implements host imports (`llm`, `config`, `logging`), enforces resource limits |
| `chatty-module-registry` | Discovery (`scan_directory`), lifecycle (`load`/`unload`/`reload`/`watch`), manifest parsing |
| `chatty-protocol-gateway` | HTTP server (axum) exposing modules via OpenAI, MCP, and A2A protocols |
| `chatty-core` | A2A client (`A2aClient`), agent tools (`list_agents`, `invoke_agent`), settings models and repositories |

### WIT contract

The host–guest interface is
[`wit/chatty-module.wit`](https://github.com/boersmamarcel/chatty2/blob/main/wit/chatty-module.wit)
(package `chatty:module@0.1.0`); [wit-reference.md](wit-reference.md) has the full
type reference.

**Host imports** (what the host provides to the module):

| Interface | Function | Purpose |
|:----------|:---------|:--------|
| `llm` | `complete(model, messages, tools)` | Run an LLM completion via host-managed API keys |
| `config` | `get(key)` | Read key-value config from the module's manifest |
| `logging` | `log(level, message)` | Emit structured logs; forwarded as A2A progress events by the gateway |

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

### Module manifest (`module.toml`)

```toml
[module]
name = "echo-agent"
version = "0.1.0"
description = "A simple echo agent for testing"
wasm = "echo_agent.wasm"        # Relative to this file's directory
# execution_mode = "local"      # "remote" modules run on the hive-runner; no local .wasm

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

`[protocols].a2a = true` is what makes a module invocable as an agent from
conversations. Without it the module can still serve tools via MCP or completions via
OpenAI-compat, but it does not appear in `list_agents` output. The registry skips
`execution_mode = "remote"` modules during its WASM scan; the gateway routes those to
the hive-runner instead.

### Resource limits

Every module runs inside a sandboxed Wasmtime instance with three enforcement
mechanisms (`crates/chatty-wasm-runtime/src/limits.rs`), overridable per module via
`[resources]`:

| Limit | Default | Purpose |
|:------|:--------|:--------|
| **Fuel** | 100,000,000 units | CPU budget (≈1 unit per Wasm instruction) |
| **Memory** | 64 MiB | Linear memory cap |
| **Timeout** | 300,000 ms (5 min) | Wall-clock execution limit |

## Protocol gateway

The gateway (`chatty-protocol-gateway`) is a local HTTP server on
`http://localhost:<gateway_port>/` (default `8420`, **Settings → Modules**) that
exposes every loaded module through three protocols at once:

| Method | Path | Protocol | Description |
|:-------|:-----|:---------|:------------|
| `GET` | `/` | — | JSON index of all modules and endpoints |
| `GET` | `/.well-known/agent.json` | A2A | Aggregated agent card (modules and participants) |
| `GET` | `/a2a/{module}/.well-known/agent.json` | A2A | Per-module agent card |
| `POST` | `/a2a/{module}` | A2A | JSON-RPC: `message/send`, `message/stream`, `tasks/get` |
| `POST` | `/v1/{module}/chat/completions` | OpenAI | Module-specific chat completion |
| `POST` | `/v1/chat/completions` | OpenAI | Model-routed (`model: "module:{name}"`) |
| `POST` | `/mcp/{module}` | MCP | JSON-RPC: `tools/list`, `tools/call`, `initialize` |
| `GET` | `/mcp/{module}/sse` | MCP | SSE transport |

### A2A via the gateway

For a local module, `invoke_agent` constructs an `A2aAgentConfig` whose `url` is
`http://localhost:<port>/a2a/{module}` (no API key, `skills` = the module's tools) and
calls the same `A2aClient::send_message_stream()` used for remote agents.

The gateway's `message/stream` handler (`handlers/a2a.rs`):

1. Emits `{"status": {"state": "working"}, "final": false}` immediately
2. Runs the module's `chat()` on a blocking task
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
process that connected to the gateway's Unix socket, published an agent card
and answers tasks over that socket. ADR-0011 routes all fleet coordination —
local and hosted — through this one broker rather than through a second
fan-out path, so a child process and a WASM module are the same thing to an
A2A caller. Participants are looked up **first**, so a live process shadows a
module of the same name.

The socket carries newline-delimited JSON frames (`register`, `task`,
`status`, `artifact`, `cancel`, `input`), which the gateway maps onto the same A2A
status and artifact updates a module produces. The connection is the liveness
signal: closing it deregisters the participant and fails every task it still
owed. A worker's `ask_user` parks its task in `input-required` with the
question attached; the caller answers with `message/send` on the same task id
and the broker hands the answer down as an `input` frame (AGE-306). The frames
and the mapping are documented in
[`crates/chatty-protocol-gateway/README.md`](../crates/chatty-protocol-gateway/README.md#local-participants).

Opening the socket is opt-in (`ProtocolGateway::with_participant_socket`) and
Unix-only. The hosted transport is Firecracker vsock, which arrives here as an
ordinary stream: both `serve_connection` and `ParticipantConnection` take any
`AsyncRead + AsyncWrite`, so the frames, the registration and the liveness rule
are shared rather than reimplemented (AGE-307, in `boersmamarcel/hive`).

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
nothing. `AgentOrigin` lives on both sides of the seam — `chatty-protocol-gateway`
serves it, `chatty-core` reads it — and a test in the gateway (which has `chatty-core`
as a dev-dependency) pins the two spellings against each other.

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

### `local-agent` — a chatty agent in its own process

`invoke_agent { "agent": "local-agent", "prompt": "…" }` asks the broker for a worker.
The gateway spawns `chatty-tui --participant-socket … --participant-name …`, the child
registers, and its turn comes back as A2A status and artifact updates. This is
One fan-out path, a public wire format, and a place to put discovery, budgets and the
ledger.

The child maps its `SessionEvent`s to frames with
`chatty_protocol_gateway::worker::TaskMapper` (the `worker` feature) — tool starts and
finishes become `working` status messages, assistant text becomes artifact chunks, and
the turn's token usage rides in the terminal status's `metadata` (A2A has no usage
concept; usage belongs to the ledger). The mapper and the one-task loop around it live
beside the broker's own half of the protocol, not in this crate, because a microVM's
`chatty-server` is a worker too and the parent must not be able to tell the two apart.
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
(ADR-0012), through `chatty_core::services::worker_tree`.

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

**Known limitation.** The worker's model is its own configured default, not the parent
conversation's: the model would have to ride on the A2A request and A2A has no field
for it. Carried as an open question on AGE-301.
