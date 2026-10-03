# Agents and specs

**When to read this:** You need to know how a conversation reaches an agent — a remote
A2A service or a local agent spec the broker runs — how `list_agents`, `invoke_agent`
and `/agent` find it, and where the broker and gateway fit.

WASM plugins are not agents: they are tools inside one, and have their own page,
[plugins.md](plugins.md).

## Overview

Chatty supports two kinds of agents that can be invoked during a conversation:

| Agent type | Where it runs | How it's called | Configured in |
|:-----------|:--------------|:----------------|:--------------|
| **Remote A2A** | External HTTP service | Direct HTTP to the remote URL | Settings → Extensions |
| **Local agent** | A `chatty-tui` worker the broker spawns from an agent spec | Via the local Protocol Gateway / broker | Agent specs (`.chatty/agents/`), listed on Settings → Agents |

Both are **unified behind the same tools** (`list_agents`, `invoke_agent`) and the
same **A2A JSON-RPC protocol**, so the LLM does not need to know which kind it is
talking to.

There is **one kind of local agent** (PL-U5): a spec, run by chatty's harness, whether it
came from the workspace, the data directory or the presets. A WASM module is **not** an
agent (PL-D1 option B): it is a plugin, whose tools an agent spec loads into its own
agent ([plugins.md](plugins.md)). No code path lists, routes or invokes a module as an
agent, and `module.toml` refuses the keys that used to say it was one.

### The local roster

The broker serves the **roster**: the specs `module_settings.json`'s `virtual_agents`
names (a `--team` names its own), or — when nothing is declared — `local-agent` and
**every exposed spec of your own** (workspace and data directory), first definition of
each name (`chatty_core::agent_spec::{load_roster, roster_names, exposed_specs}`). The
presets are experimental teams' roles and join the default roster only when one of
your specs names them in `delegates_to`, directly or through another preset (a glob
pulls in none; AGE-760). Otherwise a preset runs with its team (`--team <id>`) or when
`virtual_agents` names it: then the `data-analyst` preset is an agent like any other,
`list_agents` lists it, `invoke_agent` reaches it at `/a2a/data-analyst`, and
`/agent data-analyst …` runs it. A spec that names its `callers` (a team-internal worker
such as `panel-writer`) is served for its lead but never offered to the root's
`list_agents`/`invoke_agent`. A spec file that does not load is left out of the roster
with a warning; Settings → Agents and `/agents` show it with its error.

On the desktop, "the workspace" a conversation's own `list_agents`/`invoke_agent`/`/agent`
resolve the roster from is its own working directory when it has one, else the shared
default (`chatty_core::agent_spec::roster_workspace`) — the same value the broker itself
resolves from when its gateway (re)builds. A conversation whose own resolved workspace
does not match the workspace the *running* broker was actually built for gets no local
agents rather than one it cannot reach (`roster_workspace_matches`, AGE-719): no broker
running yet is not a disagreement, but a live one built for a different workspace is.

### `/agent` and `/agents`

`/agent <name> <prompt>` resolves `<name>` the same way in both frontends
(`chatty_core::services::agent_command::resolve_agent_command`): an enabled remote A2A
agent first (as in `invoke_agent`), then a spec on the local roster; otherwise the whole
text is the prompt for the default sub-agent. A remote agent is called over A2A. On both
the desktop and chatty-tui, a spec — and the default sub-agent, as `local-agent` — is a
turn of the conversation handed to that agent through the conversation's own broker
(`TurnInput::delegation`, `chatty_core::session::Delegation`, AGE-744/AGE-747): the model
is not asked, the turn is the one `invoke_agent` call, so the delegation row and its swarm
tree show exactly as for a model-issued call, and a worker that fails or exits ends the
row with its error. It needs the conversation's broker, like `invoke_agent`; the desktop
publishes one whether or not the module runtime is on (AGE-759). There is no
subprocess path: `chatty-tui --agent <name>` is still how you start the terminal app
itself running as that spec, but `/agent` inside a running conversation never shells out
to it. The desktop's lazy broker hands the root its gateway's direct transport
(`LazyGatewayBroker::transport`), as a `--broker` chatty-tui root's does. chatty-tui's `/agents` lists the remote
agents, the roster's specs with model, profile, plugins and grants, and every spec file
the roster leaves out with the reason; the desktop shows the same on **Settings →
Agents** (read-only: edit a spec in its TOML file, then Reload).

## Remote A2A agents

### Configuration

Remote agents are added under **Settings → Extensions** (listed again on Settings →
Agents) and persisted to `a2a_agents.json` via `A2aJsonRepository`.

**Data model** (`A2aAgentConfig` in `crates/chatty-core/src/settings/models/a2a_store.rs`):

```rust
pub struct A2aAgentConfig {
    pub name: String,           // User-visible name, also the invocation key
    pub url: String,            // Base URL (e.g. "https://hive.dev/a2a/voucher-agent")
    pub api_key: Option<String>,// Optional Bearer token
    pub enabled: bool,          // Toggle on/off
    pub skills: Vec<String>,    // Cached from agent card discovery
    pub allow_private_network: bool, // AGE-806: opt this agent into RFC-1918/CGN/ULA
}
```

Runtime connection status is tracked in `A2aAgentsModel` (a GPUI global) but **not
persisted** — it is refreshed at startup by fetching agent cards.

### Reaching a private network (AGE-806)

Every call goes through the SSRF guard's `GuardedResolver`
(`crates/chatty-core/src/services/ssrf_guard.rs`) with an `AddressPolicy` chosen per
call from `config.allow_private_network` (`A2aClient::http_for`), not once at client
construction: `a2a_peer` (default) or `a2a_peer_with_bypass(true)` (opted in). Both
resolve a name once and check the addresses actually dialed — never a separate
pre-check — so the rebinding protection from AGE-537/AGE-767 holds either way; the
opt-in only widens which addresses the check admits.

With the opt-in, a *name* that resolves into RFC-1918, `100.64.0.0/10` (CGN/Tailscale)
or ULA is admitted, using the same bypass semantics as the browser tool's
per-workspace toggle (`check_public_host_with_bypass`, AGE-459). A configured IP
literal was already admitted unconditionally before this flag existed (the address is
the user's explicit choice, not resolved). Link-local (`169.254.0.0/16`, cloud
metadata, and `fe80::/10`) is refused regardless of the flag, and `localhost` still
means loopback and nothing else. The AGE-756 TLS rule is unchanged: plain `http://`
only ever works for a loopback host string, so a private-network agent reached by name
needs `https://`.

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

## Protocol gateway

The gateway (`chatty-protocol-gateway`) is a local HTTP server on a Unix
socket, never a TCP port (ADR-0021 § 4): `gateway.sock` (the desktop) or
`gateway-<pid>.sock` (a `--broker` leader) in `$XDG_RUNTIME_DIR/chatty-run`,
else `<cache dir>/chatty-run`. That directory is created `0700` and checked
before use (owned by this user, no group or other bits), or the gateway does
not start. Every route requires the per-launch token, which is written to
`gateway.token` (`0600`) beside the socket and never passed in argv. It serves every loaded plugin's tools over MCP
([plugins.md](plugins.md#serving-a-plugin-over-mcp)), and the broker's agents (local
participants and virtual agents) over A2A:

| Method | Path | Protocol | Description |
|:-------|:-----|:---------|:------------|
| `GET` | `/` | — | JSON index of all modules and endpoints |
| `GET` | `/.well-known/agent.json` | A2A | Aggregated agent card (participants and virtual agents) |
| `GET` | `/a2a/{agent}/.well-known/agent.json` | A2A | Per-agent card |
| `POST` | `/a2a/{agent}` | A2A | JSON-RPC: `message/send`, `message/stream`, `tasks/get` |
| `POST` | `/mcp/{module}` | MCP | JSON-RPC: `tools/list`, `tools/call`, `initialize` |
| `GET` | `/mcp/{module}/sse` | MCP | HTTP+SSE transport: the event stream |
| `POST` | `/mcp/{module}/sse?sessionId=…` | MCP | HTTP+SSE transport: a client message (answered on the stream) |

A plugin is served over MCP only, and only when its `[protocols] mcp` is set (otherwise
404). There is no OpenAI route and no A2A route for a plugin: it has tools, not a loop
(PL-U3). Every route refuses a non-loopback `Host` or `Origin` with 403 (DNS
rebinding) and a body over 10 MiB with 413. Each module has its own lock, so a slow
call to one never holds up another; a guest reply over the 1 MiB output cap is a 502.
`tools/call` hands the guest the `arguments` object JSON-encoded once, as
`tool-call-request.arguments-json`. The per-route details are in
[`crates/chatty-protocol-gateway/README.md`](../crates/chatty-protocol-gateway/README.md#what-every-route-enforces).

### A2A via the gateway

`/a2a/{agent}` reaches a local participant or a virtual agent (below); a message's
text parts are all joined into the task's prompt. A module whose Hive metadata says
`execution_mode = "remote"` is still forwarded to the hive-runner until PL-H8b removes
that path.

`GET /.well-known/agent.json` returns a gateway-level card
(`{"schema_version": "0.1", "gateway": true, "agents": [...]}`) listing every
registered participant and virtual agent, each with its `origin`. A plugin is never on it.

### Local participants (ADR-0011)

`{agent}` in the A2A routes above also resolves a **local participant**: a
worker process the broker spawned on a connection it made for it, which
published an agent card and answers tasks over that connection. ADR-0011
routes all fleet coordination — local and hosted — through this one broker
rather than through a second fan-out path, so a child process and a hosted
worker are the same thing to an A2A caller. Participants are looked up
**first**, so a live process shadows a virtual agent of the same name.

**The connection is the identity** (ADR-0020, AGE-635). The broker admits a
node, which names it `<spec>-<n>` (`local-coder-0`), creates a `socketpair`
and hands one end to the child at descriptor 3 (`--participant-fd 3`); the
child marks it close-on-exec as `main`'s first statement, so no shell or tool
it starts inherits it. The child's `session.hello` names nothing — its
card's `name` is ignored — and the hello's result tells it its name, scope
and owner. Nothing registers any other way: the shared participant socket
stays bound and refuses every connection (a hello with an `error` for its
id, anything else by closing it), so no local process can take a name the
broker is about to route a task to.

The connection carries newline-delimited JSON, protocol **v3** (ADR-0021 § 1,
one `FrameCodec` per connection): every line is a request
`{"v":3,"id":…,"method":…,"params":…}`, a result `{"v":3,"id":…,"result":…}`,
an error `{"v":3,"id":…,"error":{"kind":…,"message":…}}` or a notification
`{"v":3,"method":…,"params":…}`. Each side numbers its own requests; results,
errors, `req.progress` and `task.event` name the receiver's request, and
`req.cancel` the sender's. The broker sends work as `task.run`; the worker
reports on it with `task.event` (`kind` `status`, `artifact` or `swarm`) and
ends it with the `task.run`'s result, and the broker stops it with
`req.cancel`. A method the peer may not send, a reused in-flight id or a line
that does not decode closes the connection without a reply; a response
naming nothing in flight is dropped. Every payload is typed
(`chatty_fabric::wire`): an unknown field, a duplicate key or an unknown
error `kind` does not decode either. There is no older version to fall back
to. The gateway maps a task's messages onto A2A status and artifact updates.

**Workers call over the same connection (ADR-0020, BI-4, AGE-636).** A
worker's `invoke_agent`, `list_agents` and `send_message` reach local roles
and the broker's directory as requests on its own connection, never over
loopback HTTP:

| Direction | Message | Fields |
|---|---|---|
| worker → broker | `agent.invoke`, `agent.list`, `mailbox.post` requests | `id` (the worker's, never reused on the connection), `params` (none for `agent.list`) |
| broker → worker | `req.progress` notification | `id`, `event` — `{"Admitted": "…"}` once the callee's node is admitted, `{"Step": "read_file"}` for a line about the callee's work, `{"Text": "…"}` for its answer as it streams |
| broker → worker | result | `id`, `result` — for `agent.invoke` `{success, response, error?, metadata?}`, the callee's terminal status as an A2A caller reads it (usage, trace, conversation and evidence ride in `metadata`); for `agent.list` the aggregated card's `agents` array; for `mailbox.post` `{"status":"pending","id":"msg-1"}` or `{"status":"refused","reason":"not_on_tree"}` (see [`send_message`](#send_message)) |
| broker → worker | error | `id`, `error: {kind, …, message}` — the call could not run (`unknown_agent`, `refused`, `spawn_context_refused`, `delegation`, …); the kind's fields ride beside it and `message` is display text only |
| worker → broker | `req.cancel` notification | `id` — the worker withdraws its call, its approval or its question |
| worker → broker | `human.approve` request | `id`, `params: {kind: exec\|write, command_or_path, diff_stat?}` — an approval only the root answers (EN-2a); the result is `"approved"` or `"denied"`. The broker stamps the asker itself and never sends this to a worker |
| worker → broker | `human.ask` request | `id`, `params: {questions: [{id, question, options}], origin?}` — a question (EN-2b); the result is the answers, `[{id, answer, custom}]`, or an error when nobody up the chain answered. The broker stamps the asker itself |
| broker → worker | `human.ask` request | `id` (the broker's), `params: {question, request}` — a callee's question relayed to its caller, `question` the broker's id for it and `request` as the asker sent it, asker stamped; the result is `{"answers": […]}` or `"escalate"` |
| broker → worker | `req.cancel` notification | `id` — the broker withdraws a relayed `human.ask` (or a `task.run`) |

```text
worker → {"v":3,"id":2,"method":"agent.invoke","params":{"agent":"local-reviewer","prompt":"review it","handle":null,"include_trace":false}}
broker → {"v":3,"method":"req.progress","params":{"id":2,"event":{"Step":"read_file"}}}
broker → {"v":3,"id":2,"result":{"success":true,"response":"Looks good.","metadata":{…}}}
```

The call says nothing about its caller: the broker runs it as the node the
connection names (whose endpoint permit it releases, the edge log's `from`), and
writes one edge-log row per `invoke_agent` call when it ends and one `message`
row per `send_message` call (`<data_dir>/chatty/fabric/edges-<pid>.jsonl`;
`list_agents` is a directory read, not an edge, and writes none). Several calls can be in flight in one
task; replies match by `id`, in whatever order the calls finish. A callee
whose task failed is a result with `success: false`, not an `error`, so the
caller renders it exactly as a failed A2A task. When a
worker's connection closes, the broker cancels every call still in flight on
it, which reaps the workers those calls started — so cancelling a leader's
task reaps its whole subtree (invariant 11).

**A question is a request that climbs the caller chain (ADR-0021 § 2, EN-2b,
AGE-771).** A worker's `ask_user` parks its task on a `human.ask` request and
gets the answers as its result. The broker overwrites the request's `asker`
with the connection's admitted name and the chain of the call running it, gives
the question an id of its own (`question-N`), and relays it to whoever made that
call: a calling worker gets it as a broker→worker `human.ask`, the root as
`CallEvent::Ask { id, request }`. A worker has no human, so it answers every
relayed question `escalate`, and the broker forwards the *original* request,
first stamp intact, to the next caller up — the root sees the agent that asked,
never a relayer (`escalated_question_keeps_first_stamp`). The root's
`invoke_agent` asks it on its own clarification store, each question headed by
the stamp and shown literally (`ClarifyingQuestion::forwarded`), and answers with
`Transport::answer(id, answers)`, which reaches only the request that asked. A
question a worker relays from a third-party A2A peer carries that peer's
`origin`, set by `invoke_agent`'s client code through `Transport::ask`, never by
`ask_user`'s arguments. When the asker withdraws its question (`req.cancel`), or
the call running it ends or is cancelled, the broker sends `req.cancel` for the
copy relayed to a worker, or `CallEvent::InputWithdrawn { id }` for one at the
root (`callee_cancel_withdraws_relayed_question`):

```text
leaf   → {"v":3,"id":2,"method":"human.ask","params":{"questions":[{"id":"q1","question":"Which database?","options":[]}]}}
broker → {"v":3,"id":3,"method":"human.ask","params":{"question":"question-1","request":{"questions":[…],"asker":{"agent":"leaf-0","chain":["root","mid","leaf"]}}}}
mid    → {"v":3,"id":3,"result":"escalate"}
broker → {"v":3,"id":2,"result":[{"id":"q1","answer":"SQLite","custom":false}]}
```

**The root sees every nested run (TB-1, AGE-663).** A run a worker's call
starts is nested under the root's delegation. When the root's call is
listening — every root `invoke_agent` over its broker is — the broker sends
that run's `task.run` with `"swarmEvents":true`, and the worker reports its
own turns and tool events as `task.event`s of kind `swarm` beside its usual ones
(`{"v":3,"method":"task.event","params":{"kind":"swarm","id":1,"event":{"kind":"tool_call_started","id":"call-1","name":"read_file"}}}`).
The broker forwards them to the root's call, tagged with
`(root_task_id, node, chain)` from its own task table — never from the event,
whose extra fields are dropped when it is parsed — together with what it reads
off the run's own frames: its text as a byte count (`{"kind":"text","bytes":15}`;
the text itself stays with the run's caller), its usage whole, and its end.
It batches per node and flushes at most once every 250 ms, one batch per node,
the last before the root's result; nothing is forwarded per token. The root's
session emits each batch as `SessionEvent::SwarmEvent` — a view of the tree,
not a bill: the delegation's own progress and usage still arrive as
`SessionEvent::Delegation`. A task nobody forwards gets no `swarm` events, so
its wire is unchanged (`nested_events_reach_the_root_tagged`,
`forwarding_is_bounded`, `worker_cannot_forge_tags`).

**The swarm tree and its ATIF export (TB-2, AGE-664).**
`chatty_core::services::swarm_trace::SwarmTrace` folds a turn's
`SessionEvent`s — the root's own tool calls and usage, and every
`SwarmEvent` batch — into a tree of `AgentNode {name, spec, model, turns,
tool_calls, usage, status}` rooted at the turn. The root's own callee is not
forwarded; it is built from its delegation's progress: its node from
`Started`, its usage from `Finished`, and its tool calls from the step lines
less those its descendants' batches account for (the steps carry the
descendants' too). Its calls carry no id, arguments or result across the hop,
and it is named by its spec until the edge log names its node. A frontend
builds the tree live:
`SwarmTrace::new()`, then `apply(&event)` for each event in order, redrawing
when `revision()` moves. After the turn the broker's edge log rows
(`apply_edge`, or `SwarmTrace::from_edges(rows, events)` in one call) place a
run under the right one of two same-spec siblings and add the calls a worker
had refused as `Refused` nodes. A run reports its usage once, with its own
workers' already folded in (AGE-415), so a node's own spend is what it
reported less what its children reported, per model: nobody is billed twice,
and the nodes sum to what the root's conversation records
(`swarm_tree_spend_sums`). `exporters::export_swarm` writes the tree as one
ATIF document: a step per tool call with `extra.agent` naming its agent, a
plugin tool's call (`<plugin>__<tool>`) with `extra.plugin` naming its
plugin, and the agents with their parents and own usage — tokens per model,
never a price — in `extra.swarm`. `swarm_tree_from_atif` reads it back into
the same tree (`swarm_atif_round_trip`).

**Spawn context: sub-leaders use the root broker (ADR-0020 invariants 5–6,
BI-5, AGE-637).** Only a root process starts a broker. A sub-leader — a worker
whose spec delegates in turn — is spawned with no `--broker`, and
`--broker` inside a worker (`--participant-fd`) starts nothing: its calls go
over its connection like any worker's. What its own broker used to decide
travels on the spawn request as a `SpawnContext {workspace_root, base_branch,
roster, verification, endpoint}` (on the `invoke_agent` call's `params` as
`spawn_context`, and on the child's `task.run`). The broker sets it from the
calling node's own context: the node's own worktree and branch and the roster
it was given, with the verification command and endpoint the root's settings
give the agent being spawned. So a sub-leader's worker gets its tree under the
sub-leader's tree, on a branch that starts at the sub-leader's branch, and its
evidence diffs against that branch (`grandchild_branches_from_its_subleader`).
A call that brings a context of its own is clamped: accepted only if
`workspace_root` lies inside the caller's own tree, `base_branch` is the
caller's, `roster` is a subset of the caller's, `verification` is the root's
(or none) and `endpoint` is the root's for that agent. Anything else ends the
call with an `error` of kind `spawn_context_refused`, whose `message` is
`{field, reason}` naming the field (`spawn_context_is_clamped`), so a
`team.json` a model wrote into its worktree cannot loosen anything. A call to
a virtual agent outside the caller's roster is `refused`. Usage folds up both
hops through the root (`usage_folds_across_two_hops`).

A worker **connects before it builds its agent**: `chatty-tui
--participant-fd` says `session.hello`, gets its result, and only then builds the agent
with the connection's transport (`WorkerConnection::transport`,
`AgentBuildContext::fabric_transport`), so its tools hold the connection from
the first turn. The in-process chatty-tui root reaches its own broker the
same way minus the socket: `LazyBroker::transport` hands `invoke_agent` and
`list_agents` a `DirectTransport` into the broker. Remote agents stay on
`A2aClient`. The gateway counts HTTP requests for roles and
the directory (`RouteCounter`); in a swarm of workers both stay at zero
(invariant 4, `no_worker_call_uses_loopback`). The connection is the liveness
signal: closing it deregisters the participant and fails every task it still
owed. A worker's `ask_user` parks its task on a `human.ask` request the broker
relays up the caller chain, and the answers come back as its result (EN-2b). The messages
and the mapping are documented in
[`crates/chatty-protocol-gateway/README.md`](../crates/chatty-protocol-gateway/README.md#local-participants).

The participant path is Unix-only. The hosted transport is Firecracker vsock,
which arrives here as an ordinary stream: both `serve_connection` and
`ParticipantConnection` take any `AsyncRead + AsyncWrite`, so the frames, the
hello and the liveness rule are shared rather than reimplemented
(AGE-307, in `boersmamarcel/hive`; hive moves to v3 with HS-4a, after ADR-0021
step 3).

## LLM-facing tools

`list_agents` and `invoke_agent` are registered by `AgentFactory` in **every**
conversation.

### `list_agents`

One flat list of everything addressable, each entry saying whose machine it runs on:

```json
{
  "agents": [
    { "name": "leased-vm", "origin": "fleet", "kind": "worker", "description": "...", "enabled": true },
    { "name": "local-agent", "origin": "local", "kind": "worker", "description": "...", "enabled": true },
    { "name": "voucher-agent", "origin": "remote_configured", "kind": "remote", "url": "https://...", "enabled": true, "has_api_key": true, "skills": ["..."] }
  ],
  "total": 3,
  "note": "To invoke an agent, use the `invoke_agent` tool..."
}
```

Two sources feed it. **Settings** give the configured remotes and the roster's specs
(names only, as a stand-in until the broker answers). The **broker's aggregated card** gives whatever registered since — a worker
spawned a minute ago is addressable, and only the broker knows it exists. A name in both
keeps the settings label, because what the user configured is the more informative
answer. A gateway that is off or slow to answer is not an error: the list is then what
settings know. API key values are **never exposed** to the LLM — only
`has_api_key: true/false`.

#### `origin` — whose machine it runs on (ADR-0011 C5)

| Origin | Means | Inside the fleet? |
|:-------|:------|:------------------|
| `local` | a process on this machine: a spawned worker running a spec | yes |
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
{ "agent": "local-agent", "prompt": "Hello, agent!" }
```

Resolution order: remote A2A agents first (a remote agent shadows a local spec with
the same name), then the roster's specs — `local-agent` and the rest, the broker's local
workers (below). Any other name is `NotFound`, listing what is available; a plugin's name
is never an agent's. Every path
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

### `send_message`

A worker tells the agent that gave it its task something besides its final
answer (tree messages, TM-1, AGE-654). The tool is registered for **every
worker with a broker-made connection**, a leaf `coder` or `reviewer` with no
`delegates_to` included — it needs an owner, not delegation rights — and every
tool profile allows it. The root has no owner and no tool.

```json
{ "to": "root", "text": "the tests pass; starting on the docs" }
```

It travels as a `send_message` call on the worker's connection and returns at
once: `{"status": "pending", "id": "msg-1"}` or `{"status": "refused",
"reason": …}`. It never starts a run and never interrupts one.

- **Recipient.** The broker reads the sender from the connection and its owner
  from the directory; `to` must be that owner's name. The owner is whoever
  spawned the worker: the node whose `invoke_agent` started it, recorded when
  the broker admits it, or `root` (`ROOT_NAME`) when the root asked. The
  hello's result says which, and the `to` parameter's description repeats it; a
  sub-leader's workers message the sub-leader, never the root past it. A sibling, the sender itself, a name nobody
  has or a node of another conversation is `not_on_tree`; an owner that has
  ended is `recipient_ended`. Messages to a worker's live handles come with
  resumable conversations (RC-3).
- **Bounds.** An accepted message waits on the recipient's
  `chatty_fabric::PendingList`: at most 64 KB per recipient, and at most 8 KB
  per sender per run of the recipient. A message over either bound is
  `over_allowance` — refused whole, never truncated.
- **Delivery** (TM-2, AGE-655) happens at exactly two points, never mid-run:
  appended to the next `invoke_agent` result the recipient receives, as
  `messages: [...]` in `InvokeAgentOutput` (absent when empty; a failed
  delegation lists them after its error), or prepended to the recipient's next
  run — the root's next human turn (`Transport::take_run_messages` via
  `LazyBroker::take_run_messages`, kept in history with the turn), a node's
  next task text. Each message is delivered once, as untrusted data:

  ```
  <message from="local-coder-2" untrusted="true">…</message>
  ```

  with `<` and `>` in the body escaped (`chatty_fabric::wrap_message`). A
  message grants nothing: the recipient's tools, budget and approval policy
  are what they were.
- **Drops.** When the recipient ends, what is still waiting for it is dropped,
  one edge-log `message` row per message with outcome `dropped`.
- **Neutral description.** The description says what the tool does and names
  neither relaying nor siblings (golden:
  `crates/chatty-core/src/tools/goldens/send_message_description.txt`): the F3
  gate counts messages that name another worker, and must count demand, not
  instruction.

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

A worker's `ask_user` does not end at the worker. It is a `human.ask` request
the broker relays up the caller chain, each worker escalating it, until it
reaches the root's `ask_user` popover with the asking worker named; the answers
come back as the request's result (ADR-0021 § 2, EN-2b). `crates/chatty-tui/src/participant/input_required_chain.rs`
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

A worker works in its caller's tree — the conversation's workspace, passed as its
`--workspace` and `cwd` — unless its team asks for isolation (AGE-822): `isolate` in the
`team.json` that claims the agent by name, else `module_settings.team.isolate`. Off by
default; the coding preset `fix-and-verify` turns it on. Only then does the embedder hand
the runner a workspace factory, and only then does BI-5's nesting below apply. An
isolated worker runs in its own `git worktree` under the conversation's workspace
(ADR-0012), through `chatty_core::services::worker_tree`: `.chatty/worktrees/<name>`
on branch `sub-agent/<name>`, where `<name>` is the participant name (`local-coder-0`)
unless that branch or directory already exists in the repository — another root
process on the same repository counts its own workers from zero, and a tree left from
an earlier run keeps its branch — in which case it is the first free `<name>-N`
(AGE-402). A sub-leader's worker gets its tree under the sub-leader's, branched from
the sub-leader's branch, and its evidence is measured against that branch (BI-5). The evidence envelope
appended to the worker's answer names the branch actually created. In a repository, a
tree that cannot be made fails the delegation — a repository with no commits yet fails
before anything is made, since its branch is unborn — and only a workspace that is not
a repository runs an isolated team's workers unisolated.

A worker that never started — its tree, its spawn or its hello failed — is a setup
problem only the user can fix (AGE-822). The broker's result for it carries the typed
`worker_start_failed: '<agent>' could not be started: <why>`
(`chatty_fabric::worker_start_failed`); `invoke_agent` turns it into the terminal,
non-retryable `InvokeAgentError::WorkerStartFailed`; the `StopOnWorkerStartFailure` hook
(`services::worker_start`) ends the caller's run at its next model call, with a
**Could not start** card (what failed, what to do) as its answer; a sub-leader's
`TaskMapper` fails its own task with the same typed text, so every caller up the tree
stops; and the desktop draws the card as an error alert. `/agent` checks the same way
before it spawns anything (`agent_command::preflight`): no workspace, or code execution
off, is a card instead of a delegation, and a turn that goes ahead shows its workspace
under the command.

**Per-endpoint concurrency budget (ADR-0011 C6).** Workers all talk to the same model
server, so the broker holds a semaphore per *endpoint* — the server's base URL, not a
model and not a worker — and a task waits for a slot before a child is spawned. On a
local Ollama, three concurrent workers on one loaded model is not three times the
throughput; it is the fourth request evicting the weights the first three are using.
Waiters are served first come, first served.

**A permit covers a run's model calls, not a worker's whole life (ADR-0020 §3.5,
BI-6).** A run takes its slot just before its worker is spawned and holds it while it
talks to its model. When it calls another agent (`invoke_agent` over its connection),
the broker releases its slot until the call is answered; the result that brings the
run's outstanding calls back to zero waits in the endpoint's queue for a slot before it
is delivered, since that result is what starts the run's next model call. So a
sub-leader and its child on the same budget-1 endpoint both complete: the sub-leader is
not holding the slot its child needs. If the run's caller gives up while that result
waits (a cancel, a deadline), the wait is dropped and the run makes no model call. The
slot is released for good when the worker is reaped. The state machine is
`chatty_fabric::RunPermit` (`Holding → Released{outstanding} → Reacquiring → Holding`)
on `chatty_fabric::EndpointBudget`.

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
the same as the desktop's: workers work in `--workspace` (or the persisted workspace),
and a team with `"isolate": true` gives each a `git worktree` there when it is a git
repository. When the turn (or the session) ends the gateway stops
serving; any worker still running is reaped by the runner the same way it always is.
`--broker` never writes module settings: its ephemeral port lives only in this run's
agent-build context, so `/modules` in the same session still saves exactly what was on
disk (AGE-382).

**The broker starts lazily, on first use (BI-2, AGE-634).** Neither host starts the
broker at boot: `--broker`/`--team` (chatty-tui) and the desktop's module settings only
prepare it — a `chatty_core::services::lazy_broker::LazyBroker` — and the
first `list_agents` or `invoke_agent` call is what actually binds its socket and TCP
port, through `LazyBroker::ensure_started`. The desktop prepares one whether or not the
module runtime is on: that switch gates WASM modules only. With it off, the broker's
gateway loads no module and binds no TCP port — the root reaches it over
`LazyBroker::transport` alone — and a broker that fails to start puts its reason in the
`invoke_agent` error (AGE-759). Later calls reuse the same broker
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
module = "benford"
version = "^0.2"                   # checked against the module's [module].version
grants = ["llm"]                   # a subset of what it requests; logging is always granted
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
`disable` and `max_agent_turns` and then gated, the skills line, and the run's
`RunBudget` from `max_agent_turns` and `cap_usd` (a `LocalSpendGate`, see
[token-tracking.md](token-tracking.md#the-local-spend-gate-dp-3)). A worker receives its whole spec on its argv as
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
| `swarm.callers` | Optional. When set, only these agents (names or `*` globs) may call it. `chatty_core::services::delegation_policy::may_call` checks both specs: the caller's `delegates_to`, then the callee's `exposed` and `callers`. The broker asks it on every worker's `invoke_agent` before anything is spawned, and on the root's when the root runs as a spec (a `--team` leader, an `--agent <spec>` root; a plain root may call anyone), then refuses a call that closes a cycle or goes deeper than 4 levels below the root; the calling model reads `Error: invoke_agent: not_listed: …`, `cycle: root → a → b → a` or `too_deep: depth 5 > max 4` (PL-S2 DP-2). |
| `budget.max_agent_turns` | Optional. The agent's own turn budget (AGE-440). Absent: an unattended run has no turn cap and a 30-minute time budget. |
| `budget.max_duration` | Optional. The wall-clock budget, as `--max-duration` writes it. |
| `budget.cap_usd` | Optional. Dollars one task may spend before `invoke_agent` refuses to start another delegation (`budget_spent: usd`). |

A called agent runs under the tighter of its own `[budget]` and what its caller has left
(PL-S2 DP-3): `turns = min(max_agent_turns, caller's turns left)`, `deadline = min(now +
max_duration, the chain's deadline)`, `usd = min(cap_usd, caller's dollars left)`, where
what the caller has left counts what it already spent, its callees' usage included. The
broker refuses a call once any of them is used up, before anything is spawned, and stops a
callee still running past its deadline (plus a tenth of the budget, 5 s to 2 min) with a
failed result.
| `plugins` | Optional. WASM modules whose tools this agent runs in-process — see [plugins.md](plugins.md#a-plugins-tools-as-the-agents-own). |

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

An empty or absent list is every exposed spec, `local-agent` first ("The local roster"
above). A role is a spec and
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
settings-configured desktop leader forwards nothing. Settings → Agents shows the roster
and every spec file; `virtual_agents` in the JSON is how to narrow it.

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
| `team.isolate` | Optional, default `false`. Whether each worker of a roster no team claims gets a `git worktree` of its own (AGE-822). |
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
| `leader` | The spec the leader runs as. `--model`, `--tools` and `--preamble` still beat its fields; `--model` also replaces every roster member's model for the run, a pinned one included (`Team::run_roster`, AGE-808). |
| `agents` | The roster, by spec name. Replaces `module_settings.virtual_agents` for the run. |
| `verification` | Optional. The team's verification command (`team.verification` above) for the run. |
| `isolate` | Optional, default `false`. Each worker gets a `git worktree` of its own instead of the conversation's workspace (AGE-822). A coding team sets it; an analysis team works where the user's files are. Copied into `module_settings.team.isolate` for a `--team` run. |
| `skill` | Optional. The skill the leader is told to follow: its first turn opens with `read_skill <skill> and follow it`, plus the verification command when one is declared, since a `coordinator` leader has no shell and can only delegate the check. `read_skill` serves the `SKILL.md` beside `team.json` ahead of the skill directories. |
| `max_agent_turns` | Optional. The leader's turn budget for the run, ahead of the leader spec's own; without either a headless leader has no turn cap and a 30-minute time budget. A worker's budget is its own spec's. |
| `handoffs` | Optional. Role → a JSON Schema its handoff must match, as a path relative to the team directory (see *Typed handoffs* below). |

A `team.json` in the old shape — a `leader` object, agent objects in `agents` — fails to
load with an error naming the field.

**Typed handoffs (TD-2, AGE-693).** A team can make what one role hands the next
explicit and checkable. `handoffs` names a JSON Schema per roster role:

```json
{
  "leader": "coordinator",
  "agents": ["coder", "reviewer"],
  "handoffs": {
    "coder": "schemas/change.json",
    "reviewer": "schemas/review.json"
  }
}
```

Every schema is read and compiled when the team loads; a missing file, a file that is
not JSON, a schema that does not compile (remote `$ref`s are not fetched), or a role
that is not in `agents` fails `--team` before anything runs. A worker running as a role
with a schema is told the schema with its task and must end its final answer with
exactly one fenced `json` block matching it:

- **valid:** the parsed JSON comes back on the leader's `invoke_agent` result as
  `handoff`;
- **invalid, the first time:** the worker gets one follow-up turn listing the schema
  errors;
- **invalid again:** the task fails and the leader's model sees
  `Error: invoke_agent: handoff_invalid: <role>: <errors>`.

A schema may add read rules, `"x-must-be-read": {"coder": ["files_changed"]}`: each
value of the coder's latest `files_changed` must appear in this role's handoff. The
leader checks them and records a miss as the `handoff_misread` failure tag, with no
retry. A headless `--team` leader's `--usage-file` carries
`handoff_invalid_by_role` (role → invalid answers) and `failure_tags`, and every run that delegates carries `delegated_by_agent` (agent → the tokens it reported), each absent when
empty. A team without `handoffs` behaves exactly as before, down to the bytes on the
worker's socket.

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

Five presets ship, all **experimental**: teams are supported, and a team becomes a
documented default only after a benchmark shows it beats a single agent (PL-S8). Their specs name no models, except
`architecture-review`'s (below), and a changed prompt is a new preset name, so a run of one stays comparable with an
earlier one.

| Team | Leader | Workers | Shows |
|------|--------|---------|-------|
| `data-analysis` | `data-lead` | `data-analyst`, `reviewer` | A business question answered with SQL (`query_data`, no shell), a reviewer that re-derives the numbers, and the saved `report.md` relayed to you for approval. `teams/data-analysis/fixture/orders.csv` is its sample: 793 orders over July–August with a known answer (August revenue −17.6%; EU `Pro` orders fall 47 → 14, about 70% of the fall; the online `SUMMER30` code takes 30% off `Starter` from 1 August without lifting volume, about 27%). |
| `research-brief` | `editor` | `researcher`, `writer`, `reviewer` | Sourced writing from a folder of documents, a relayed write approval. `teams/research-brief/fixture/docs/` is its sample corpus. |
| `fix-and-verify` | `fix-lead` | `fix-coder`, `code-reviewer` | A coding team whose tests Chatty runs (AGE-757): the coder fixes a bug in its own worktree, the team's `verification` runs the project's test command in that tree and the result goes into the `evidence` block, `code-reviewer` (no shell) judges the real diff (`git_diff <base>..<branch>`) and that evidence, never the coder's word, and the leader merges on `APPROVE` with exit code 0. At most one fix round. `teams/fix-and-verify/fixture/` is a four-test Python project with one known bug (an order of exactly the free-shipping threshold is charged shipping; the fix is `>=` in `invoice.py`). Its `verification` is that fixture's command; for your own project, put a `team.json` naming the same specs and your test command in `.chatty/teams/fix-and-verify/`. |

`data-analysis`, `research-brief` and `fix-and-verify` have no `SKILL.md`: the leader's preamble is the
playbook, so `/agent data-lead …` on the desktop runs it the same as `--team data-analysis`
once `virtual_agents` names the team (`["local-agent", "data-lead", "data-analyst",
"reviewer"]`); the desktop has no team selection of its own.
`reviewer` is shared by both.
The user walkthrough is the docs-site tutorial *From one agent to a team*; the
reliability runs behind both presets, and why `coder-reviewer` and the Benford team did
not ship, are in `docs/research/showcase-runs-2026-09-29.md`. The old `coder-reviewer`
team (`coordinator` leader, `local-coder`, `local-reviewer`, its `SKILL.md`) is now a
test fixture only, `crates/chatty-tui/tests/fixtures/team-workspace/`: `spec_golden`'s
recorded contexts and the team-mechanics tests load it from there.

```bash
chatty-tui --team data-analysis --headless --model <model> \
  -m "Revenue in orders.csv fell in August. Find out why."
```

The third preset, `analyst-panel` (experimental, AGE-754), is for data questions: a
`coordinator` leader `panel-lead` whose `delegates_to` is exactly its roster; three
identical analysts `panel-analyst-1..3` on the `coder` profile (30 turns and 12 minutes
each, inside the leader's 45-minute deadline); a read-only adjudicator
`panel-adjudicator` (the `coordinator` profile with no one to delegate to, so it compares
the analysts' traces and cannot re-solve); and `panel-writer` for a question that asks
for its answer in a file. The analysts' and the adjudicator's handoffs are typed
(`schemas/analyst.json`: `answer`, `method`, `assumptions`; `schemas/adjudicator.json`:
`choice`, `answer`, `reason`), and a preset's schemas are compiled into the binary beside
its `team.json` (`TeamPreset::schemas`). The skill has the leader delegate to the three
analysts with `include_trace`, compare their normalised answers, send anything short of
a unanimous panel to the adjudicator with every trace, and deliver the chosen answer
verbatim; a failed analyst drops out rather than being re-asked. Dataset-specific help
(helper code, conventions) is not part of the preset: the analysts read a `BRIEF.md` in
the workspace root when there is one. Measurement: `docs/research/`.

The fifth preset, `architecture-review` (experimental, AGE-808), brings one architecture
document to acceptance by repeated blank review: an ADR (`docs/adr/ADR-NNNN-slug.md`) or a
design doc (`docs/design/<component>.md`), in the frontmatter-and-headings format that
`chatty_core::services::architecture_doc` checks. It is derived from an ADR review team run by
hand. The roster:

| Agent | Profile | Model | Job |
|-------|---------|-------|-----|
| `arch-lead` | `coordinator` | `anthropic/claude-opus-5` | Picks the mode and path, runs the rounds, counts, merges the proposer's branches, asks the human product questions with `ask_user` as they come up; writes nothing. |
| `arch-proposer` | `coder` (no `execute_code`) | `anthropic/claude-opus-5` | Owns the document: reads the code itself, writes it, and verifies every finding against the code before accepting it or rejecting it with evidence. |
| `arch-maint-reviewer`, `arch-sec-reviewer`, `arch-devils-advocate` | `reviewer` | `anthropic/claude-opus-5` | Blank reviewers: a fresh instance every round with a new persona from the skill's lists, seeing neither earlier rounds nor each other. |
| `arch-verifier` | `reviewer` (no shell) | `anthropic/claude-sonnet-5` | Checks only the final polish diff (`<branch>~1..<branch>`): every hunk true, nothing meaning-bearing lost. |

The handoffs are small and flat (E8's biggest loss was invalid handoffs): `schemas/review.json`
(`must_fix`, and the review as one Markdown `findings` string, each finding tagged
`must-fix`/`should-fix`/`nit` with its claim, evidence and fix; optional `verdict`,
`should_fix`), `schemas/proposer.json` (`accepted`, `rejected`, `rejected_must_fix`,
`human_questions`, `summary`; optional `partial`, `words`, `sections_edited`) and
`schemas/verify.json` (`verdict` `PASS`/`FAIL`, `blockers`). The loop rule is the skill's: rounds
repeat until one has zero must-fix findings, not counting a must-fix the proposer rejected with
evidence; it stops as **not converged** when two consecutive rounds do not lower the count, when
a fixed must-fix comes back, or at 10 rounds. The leader merges each proposer branch
(`git_merge`, no fast-forward) before the next delegation, so the next blank reviewer reads the
current document, and merges the polish only on the verifier's `PASS`. An ADR never changes
status from `proposed`; the round log, the human's decisions, the open questions and the polish
verdict go to `<document>.review.md` beside it. The templates date themselves: `load_team` replaces
`{{today}}` in every member's preamble with the local date (`team::fill_today`), since a model asked
for today's date makes one up; the checker's `dates` rule rejects a date key that is not `YYYY-MM-DD`.
To check a document's format:

```bash
cargo run -p chatty-core --example check_architecture_doc -- docs/adr/ADR-0001-*.md
```

The run needs the git tools (`--enable git`), since the leader merges the proposer's branches with `git_merge`. It is the one preset that pins models, so it is also the one that needs a hosted provider:
`chatty-tui` checks every pinned model of the run (`team::check_model_providers`) before
anything starts, and without OpenRouter configured it fails naming OpenRouter and each agent
with its model. The same check names each agent's pin when no configured model matches it (an Azure-only
user's roster, say), with the models there are. To run the whole team on one model,
`--model <model>` replaces every member's model for that run; to change one agent's model,
shadow its spec with a `<workspace>/.chatty/agents/<name>.toml` of the same name, without
`model` (the roster's default runs it) or with your own. There is no way yet to remap only the
model of one role without copying its spec. A run on the pinned models costs hosted-model tokens for six roles
over up to ten rounds; nothing measures yet whether it writes a better document than a single
agent.

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
