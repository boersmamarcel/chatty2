# WIT Interface Reference

> **Package**: `chatty:plugin@0.4.0`\
> **Source**: [`wit/chatty-plugin.wit`](../wit/chatty-plugin.wit)

This document describes the WIT (WebAssembly Interface Types) contract between chatty (the host) and WASM plugins (guests). Every chatty plugin targets the `plugin-world` world defined here.

A plugin contributes **tools** to a chatty agent. It never runs a loop of its own: the agent that loads it (an agent spec lists it under `[[plugins]]`) offers its tools to its model as `<plugin>__<tool>`, and calls `invoke-tool` when the model asks for one (PL-D1 option B). The same tools are served to external MCP clients at `/mcp/{plugin}` when the plugin's `module.toml` sets `[protocols] mcp = true`.

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────────────┐
│                  chatty (host): an agent's harness                    │
│                                                                        │
│  ┌─────────┐  ┌──────────┐  ┌────────────┐  ┌────────┐  ┌──────────┐ │
│  │   llm   │  │  config  │  │  logging   │  │  file  │  │ billing  │ │
│  │ import  │  │  import  │  │  import    │  │ import │  │ import   │ │
│  └────┬────┘  └────┬─────┘  └─────┬──────┘  └───┬────┘  └────┬─────┘ │
│       │ one interface per capability (requested in metadata)  │       │
├───────┼────────────┼──────────────┼──────────────┼───────────┼───────┤
│       ▼            ▼              ▼              ▼           ▼       │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │                     WASM plugin (guest)                       │   │
│  │                                                                │   │
│  │  exports: plugin                                               │   │
│  │    • metadata() → name, version, requested capabilities, …     │   │
│  │    • list-tools() → definitions                                │   │
│  │    • invoke-tool(call) → result | typed error                  │   │
│  └────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────┘
```

Each host import is its own interface, one per **capability**, so the host links only the capabilities an agent granted (PL-U4). A plugin declares the ones it needs in `metadata().requested-capabilities`; an agent spec's `[[plugins]].grants` names a subset (`llm`, `config`, `logging`, `file`, `file:<root>`, `billing`), and `logging` is always granted. An import the plugin was not granted is still satisfied, by a stub that refuses: `llm`, `file` and `billing` return `Err("capability <x> not granted to this agent")` to the guest, and `config::get`, which has no error channel, ends the call with that text as its reason. Either way the module instantiates and the refusal reaches the model in the tool result. A module served on its own by the gateway (no spec) is granted `config` (if it requests it) by default, and `llm`, `file` and `billing` only when the user granted them in Settings → Plugins (SEC-11). A component that never calls an import does not import it at all, so a plugin that only computes (like `benford`) imports nothing but `logging`.

Plugins are written against [`chatty-module-sdk`](../crates/chatty-module-sdk/): implement its `Plugin` trait and call `export!(MyPlugin)`. Both are wit-bindgen's own generated code, so the export names always match this file.

---

## Shared Types (`types` interface)

### `role` (enum)

```wit
enum role { system, user, assistant, tool }
```

The role of a message in an `llm::complete` conversation. `tool` is the result of a tool call, answering the assistant message that requested it.

### `tool-call` (record)

```wit
record tool-call {
    id: string,         // unique id of this call
    name: string,       // tool name
    arguments: string,  // JSON-encoded arguments
}
```

A tool call the model asked for in an `llm::complete` reply.

### `message` (record)

```wit
record message {
    role: role,
    content: string,
    tool-calls: list<tool-call>,  // the calls an assistant message made
    tool-call-id: option<string>, // for a tool message: the call it answers
}
```

A plugin that runs a short tool exchange of its own inside one tool call feeds the model's calls back as an `assistant` message with `tool-calls`, then each result as a `tool` message whose `tool-call-id` names the call. The host sends them to the provider as a real assistant tool call and tool result, not as user text.

### `token-usage` (record)

```wit
record token-usage {
    input-tokens: u32,
    output-tokens: u32,
}
```

### `completion-response` (record)

```wit
record completion-response {
    content: string,
    tool-calls: list<tool-call>,
    usage: option<token-usage>,
}
```

What `llm::complete` returns. `usage.input-tokens` is the whole prompt (cached tokens included).

### `tool-definition` (record)

```wit
record tool-definition {
    name: string,               // unique within the plugin, e.g. "reverse"
    description: string,        // shown to the model
    parameters-schema: string,  // JSON Schema of the arguments
}
```

The model sees the tool as `<plugin>__<tool>` (e.g. `echo__reverse`); the name must keep that under 64 characters of `[a-zA-Z0-9_-]`, or the plugin fails to load into an agent.

---

## Host Imports

These interfaces are provided by chatty. They are the **only** host capabilities a plugin has: no WASI environment, arguments, preopened directories or sockets.

### `llm` — LLM Completion

```wit
interface llm {
    use types.{message, completion-response};
    complete: func(model: string, messages: list<message>, tools: option<string>) -> result<completion-response, string>;
}
```

Capability `llm`. A completion that goes through the same provider client the calling agent uses (`chatty_core::services::plugin_llm::PluginLlmProvider`), so OpenRouter, Ollama and Azure OpenAI (API key or Entra ID) all work, and every call's usage is recorded as the plugin's own line in the calling turn's totals.

**Parameters**:
- `model` — Empty (`""`) for the calling agent's model, which is what most plugins want. Otherwise a model identifier or id that must match a model configured in the host; any other name is refused with an error and no request is sent.
- `messages` — The conversation; the last message is the prompt.
- `tools` — Optional JSON-encoded array of tool definitions for the model: the flat form (`name`, `description`, `parameters`) or the OpenAI wrapped form (`{"type": "function", "function": {...}}`).

**Returns**: the completion or an error message. The call is bounded by the plugin's per-call deadline: past it the host stops waiting and returns `deadline exceeded`.

**Example** (pseudocode):
```
let messages = [
    { role: system, content: "Classify the sentiment.", tool-calls: [], tool-call-id: none },
    { role: user, content: "I love it", tool-calls: [], tool-call-id: none },
];
let response = llm::complete("", messages, none);
// response.content = "positive"
```

### `config` — Configuration

```wit
interface config {
    get: func(key: string) -> option<string>;
}
```

Capability `config`. Read the plugin's configuration: the string → string `[config]` table of its `module.toml`. A non-string value there is a manifest error.

```toml
[config]
api-key = "sk-..."
threshold = "0.8"
```

**Parameters**:
- `key` — The configuration key to look up.

**Returns**: `option<string>` — The value, or `none` if the key is not set.

**Example** (pseudocode):
```
let api_key = config::get("api-key");       // some("sk-...")
let threshold = config::get("threshold");    // some("0.8")
let missing = config::get("nonexistent");    // none
```

### `file` — Sandboxed File Reads

```wit
interface file {
    read-bytes: func(path: string) -> result<list<u8>, string>;
}
```

Capability `file`. Read a file under the plugin's **file root**: the directory its `module.toml` grants with

```toml
[files]
root = "weights"   # relative to the module directory
```

The root is host-set (the runtime's `ModuleManifest::with_weights_root`), never a `[config]` key: a config value named `weights_root` grants nothing. The plugin reads it only when its agent grants `file`; `file:<root>` grants an absolute `<root>` instead. A plugin without `[files]` and without a `file:<root>` grant can read no files. Most plugins never need this; ML inference plugins use it to load weights once at startup.

**Parameters**:
- `path` — Relative to the file root. `/` and `\` both separate components.

**Returns**: the file's bytes, or an error when:
- the plugin has no file root;
- `path` is empty, absolute, has a drive letter (`:`), or a `..` component;
- the path's resolved location (both sides canonicalized, symlinks followed) is outside the root — a symlink that stays inside the root is fine;
- it is not a regular file, or is over **256 MiB** (`MAX_FILE_READ_BYTES`, checked before reading);
- the file is missing or unreadable.

Error text never contains host paths. The read counts against the call's wall-clock limit.

**Example** (pseudocode):
```
let weights = file::read-bytes("model/weights.bin");   // ok([..])
let escape = file::read-bytes("../module.toml");       // err("... `..` component rejected ...")
```

### `logging` — Structured Logging

```wit
interface logging {
    log: func(level: string, message: string);
}
```

Capability `logging`, always granted. Emit log messages that appear in the host's log output (its `tracing` subscriber; nothing is queued for a caller).

**Parameters**:
- `level` — Log level: `"trace"`, `"debug"`, `"info"`, `"warn"`, or `"error"`.
- `message` — The log message.

**Example** (pseudocode):
```
logging::log("info", "Starting code review...");
logging::log("debug", "Analyzing 42 files");
logging::log("error", "Failed to parse input: unexpected token");
```

### `billing` — Paid Plugin Sessions

```wit
interface billing {
    record session-info {
        token: string,
        balance-tokens: s64,
        reserved-tokens: s64,
        pricing-model: string,
    }

    acquire-session: func(estimated-tokens: s64) -> result<session-info, string>;
    report-usage: func(input-tokens: s64, output-tokens: s64) -> result<_, string>;
}
```

Capability `billing`. Only paid plugins use it; a component that never calls an
import does not import it at all. A paid plugin calls `acquire-session` before doing work — the host asks Hive to
reserve credits and returns a signed session token — then `report-usage` once
the work is done, so Hive can settle the reservation against actual usage.

`hive-billing-sdk` calls these through `chatty-module-sdk`'s `billing` module
(it generates no bindings of its own), so a plugin can depend on both crates.

**`session-info.token`** is a Hive-signed JWT. The current `hive-billing-sdk`
verifies it with **HS256 (HMAC-SHA256) against a shared secret embedded in the
plugin at compile time**, not an embedded *public* key: there is no Ed25519
verification today, only a documented future upgrade path
(`crates/hive-billing-sdk/src/lib.rs`). HMAC-in-WASM is deterrence, not proof
— a determined attacker can extract the secret from the compiled plugin — and
the crate's own doc comment says so; high-trust billing should run on Hive's
Firecracker infrastructure, where verification happens server-side instead.

**Parameters** (`acquire-session`):
- `estimated-tokens` — estimated token usage for this invocation.

**Parameters** (`report-usage`):
- `input-tokens` / `output-tokens` — actual tokens consumed.

---

## Guest Export

### `plugin` — Plugin Interface

```wit
interface plugin {
    use types.{tool-definition, token-usage};

    enum capability { llm, config, logging, file, billing }

    record config-key { name: string, description: string, required: bool }

    record plugin-metadata {
        name: string,
        version: string,
        description: string,
        requested-capabilities: list<capability>,
        config-keys: list<config-key>,
    }

    record tool-call-request {
        name: string,
        arguments-json: string,
        call-id: string,
        caller: option<string>,
    }

    record tool-result {
        content: string,
        usage: option<token-usage>,
    }

    enum tool-error-kind { unknown-tool, invalid-arguments, denied, failed }

    record tool-error { kind: tool-error-kind, message: string }

    metadata: func() -> plugin-metadata;
    list-tools: func() -> list<tool-definition>;
    invoke-tool: func(call: tool-call-request) -> result<tool-result, tool-error>;
}
```

#### `metadata`

Who the plugin is and what it asks the host for. `name` and `version` match `[module]` in its `module.toml`. `requested-capabilities` is what an agent spec will be able to grant it: a grant it did not request fails the agent's build as a spec error (`plugin `<name>` is granted `<x>`, which it does not request`), and an import it was not granted answers `capability <x> not granted to this agent` (PL-U4); the host reads it at load from an instance granted nothing; `config-keys` names the `config::get` keys it reads. Hive validates this at publish (PL-H7). Budget: 1 s.

#### `list-tools`

Every tool the plugin provides. Read once, when an agent loads the plugin. Budget: 1 s.

#### `invoke-tool`

Run one tool call.

- `name` — a name from `list-tools`.
- `arguments-json` — the model's arguments object, JSON-encoded once, exactly what the tool's `parameters-schema` describes. A tool reads its own fields out of it.
- `call-id` — an id for this call.
- `caller` — the calling agent's name, when the host knows it.

**Returns** `tool-result { content, usage }` — `content` is what the model sees (JSON or plain text); `usage` is the plugin's own account of model use, informational (the host already counts every `llm::complete` call) — or a `tool-error`:

| `kind` | Meaning |
|:-------|:--------|
| `unknown-tool` | No tool of that name |
| `invalid-arguments` | The arguments did not parse or fit the schema |
| `denied` | A host capability refused (not granted, no credits, a path outside the file root) |
| `failed` | The tool ran and failed |

The model reads the error as `<kind>: <message>`, and the turn goes on. A limit (fuel, deadline, memory, output) or a trap is not a `tool-error`: the host reports it the same way, and the next call runs on a fresh instance after a trap.

**Example** (Rust, with `chatty-module-sdk`):
```rust
fn invoke_tool(call: ToolCallRequest) -> Result<ToolResult, ToolError> {
    match call.name.as_str() {
        "reverse" => {
            let args: serde_json::Value = serde_json::from_str(&call.arguments_json)
                .map_err(|e| ToolError::invalid_arguments(e.to_string()))?;
            let input = args["input"].as_str().unwrap_or_default();
            Ok(ToolResult::text(input.chars().rev().collect::<String>()))
        }
        other => Err(ToolError::unknown_tool(other)),
    }
}
```

---

## World

```wit
world plugin-world {
    import llm;
    import config;
    import logging;
    import file;
    import billing;

    export plugin;
}
```

---

## Resource Limits

Every call into a guest export (`metadata`, `list-tools`, `invoke-tool`)
runs inside a sandboxed Wasmtime instance under per-call limits
(`crates/chatty-wasm-runtime/src/limits.rs`, `ResourceLimits`). Fuel and the wall-clock
deadline are reset before each call, so a long-lived plugin never runs out of a
lifetime budget. The defaults below are also the host ceilings: a plugin manifest's
`[resources]` section may only lower a limit, never raise it — a larger value is
clamped down to the ceiling.

| Limit | Default = ceiling | Enforcement | Error |
|:------|:------------------|:------------|:------|
| **Fuel** | 10¹² units per call | Wasmtime fuel (≈1 unit per Wasm instruction) | `fuel exhausted` |
| **Wall clock** | 60 s per call, host time included | Epoch interruption (10 ms ticks); host imports (`llm`, `file`, `billing`) and WASI clock waits (`sleep`) stop waiting at the deadline | `deadline exceeded` |
| **Memory** | 256 MiB | Store memory limiter | `memory limit` |
| **Output** | 1 MiB per call | Size of each export's return value | `output too large` |

`metadata` and `list-tools` get a fixed 1 s wall-clock budget regardless of the
call's own `max_execution_ms`.

A guest that sleeps through WASI (`std::thread::sleep`: a `wasi:clocks`
subscription polled with `wasi:io/poll`) blocks inside the host, where no epoch
check runs. The host's `wasi:clocks/monotonic-clock` therefore caps every
subscription at the call's deadline (AGE-706): a wait that would end after the
deadline fires at the deadline instead, and the call fails with `deadline
exceeded`. A wait that ends in time is untouched. Clock subscriptions are the only
waits a plugin can start (it gets no sockets, files or stdin), so every WASI wait is
bounded this way.

The fuel ceiling is sized so a CPU-bound guest is bounded by the 60 s wall clock, not
by fuel (AGE-708/PL-D3b). An earlier ceiling of 10⁹ fuel — set before this was
measured — let a tight arithmetic loop exhaust its fuel in roughly 130 ms, so the wall
clock never got a chance to apply to legitimately CPU-heavy tools (parsing, a
Benford's-law audit over a large input). Measured on the reference host, Wasmtime fuel
drains at roughly 1.5 x 10^10 units/s for a pure arithmetic loop; 10¹² keeps that same
loop running for about a minute of fuel (measured: ~68 s) — past the 60 s wall clock,
so the deadline fires first. Fuel remains the deterministic bound underneath the wall
clock: it is refilled per call, and a manifest may still only lower it.

---

## Versioning

The package is `chatty:plugin@MAJOR.MINOR.PATCH`, and the host loads **exactly one** version: `chatty:plugin@0.4.0` today. A component that does not export `chatty:plugin/plugin@0.4.0` is refused at load with

```
module targets chatty:module@0.2.0; this chatty supports chatty:plugin@0.4.0 — rebuild it with the current SDK
```

naming whatever world it does target. There is no adapter for an older world and no deprecation window (PL-D1): a version change means rebuilding every plugin against the new SDK.

A version bump is deliberate. `chatty-module-sdk` and `chatty-wasm-runtime` each hold the package as a constant (`WIT_PACKAGE`) and assert at compile time that `wit/chatty-plugin.wit` declares it, so editing the file's `package` line fails both builds loudly until the constants are updated with it.

### History

| Version | Change |
|:--------|:-------|
| `0.3.0` | Plugin world (PL-U3): `plugin { metadata, list-tools, invoke-tool }` replaces `agent { chat, invoke-tool, list-tools, get-agent-card }`; typed tool calls, results and errors; plugin metadata with requested capabilities; `message.role` gains `tool`. No longer an agent: plugins contribute tools to agent specs. |
| `0.2.0` | Agent world `chatty:module`, with the `file` and `billing` imports. Refused. |
| `0.1.0` | First agent world. Refused. |
