# WIT Interface Reference

> **Package**: `chatty:module@0.2.0`\
> **Source**: [`wit/chatty-module.wit`](../wit/chatty-module.wit)

This document describes the WIT (WebAssembly Interface Types) contract between chatty (the host) and WASM modules (guests). Every chatty WASM module must target the `module` world defined here.

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────────────┐
│                            chatty (host)                              │
│                                                                        │
│  ┌─────────┐  ┌──────────┐  ┌────────────┐  ┌────────┐  ┌──────────┐ │
│  │   llm   │  │  config  │  │  logging   │  │  file  │  │ billing  │ │
│  │ import  │  │  import  │  │  import    │  │ import │  │ import   │ │
│  └────┬────┘  └────┬─────┘  └─────┬──────┘  └───┬────┘  └────┬─────┘ │
│       │            │              │        (opt) │    (opt) │        │
├───────┼────────────┼──────────────┼──────────────┼───────────┼───────┤
│       ▼            ▼              ▼              ▼           ▼       │
│  ┌──────────────────────────────────────────────────────────────┐    │
│  │                     WASM Module (guest)                       │   │
│  │                                                                │   │
│  │  exports: agent                                                │   │
│  │    • chat(req) → response                                     │   │
│  │    • invoke-tool(name, args) → result                         │   │
│  │    • list-tools() → definitions                                │   │
│  │    • get-agent-card() → card                                   │   │
│  └────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────┘
```

`llm`, `config` and `logging` are used by every module. `file` and `billing`
are optional imports — free modules never link `billing`, and only modules
that read files from a granted root (e.g. ML modules loading weights) use
`file`.

---

## Shared Types (`types` interface)

All types live in the `types` interface and are imported by other interfaces via `use`.

### `role` (enum)

Role of a message participant.

| Variant     | Description                        |
|:------------|:-----------------------------------|
| `system`    | System/instruction message         |
| `user`      | Message from the end user          |
| `assistant` | Message from the AI assistant      |

### `message` (record)

A single message in a conversation.

| Field     | Type     | Description              |
|:----------|:---------|:-------------------------|
| `role`    | `role`   | Who sent this message    |
| `content` | `string` | The message text content |

### `tool-call` (record)

A tool call requested by the LLM.

| Field       | Type     | Description                            |
|:------------|:---------|:---------------------------------------|
| `id`        | `string` | Unique identifier for this tool call   |
| `name`      | `string` | Name of the tool to invoke             |
| `arguments` | `string` | JSON-encoded arguments for the tool    |

### `token-usage` (record)

Token usage statistics for a completion.

| Field           | Type  | Description                        |
|:----------------|:------|:-----------------------------------|
| `input-tokens`  | `u32` | Number of tokens in the prompt     |
| `output-tokens` | `u32` | Number of tokens in the response   |

### `completion-response` (record)

Response from the host LLM completion API.

| Field        | Type                    | Description                          |
|:-------------|:------------------------|:-------------------------------------|
| `content`    | `string`                | The text content of the completion   |
| `tool-calls` | `list<tool-call>`       | Any tool calls the LLM wants to make |
| `usage`      | `option<token-usage>`   | Token usage for this completion      |

### `tool-definition` (record)

A tool definition that a module exposes.

| Field               | Type     | Description                                  |
|:--------------------|:---------|:---------------------------------------------|
| `name`              | `string` | Unique name for the tool (e.g. `"web-search"`) |
| `description`       | `string` | Human-readable description shown to the LLM  |
| `parameters-schema` | `string` | JSON Schema describing the tool's parameters |

### `skill` (record)

A skill that the agent can perform.

| Field         | Type           | Description                              |
|:--------------|:---------------|:-----------------------------------------|
| `name`        | `string`       | Unique name for the skill                |
| `description` | `string`       | Human-readable description               |
| `examples`    | `list<string>` | Example prompts that trigger this skill  |

### `agent-card` (record)

Metadata card describing the agent module.

| Field          | Type                   | Description                                    |
|:---------------|:-----------------------|:-----------------------------------------------|
| `name`         | `string`               | Unique identifier (e.g. `"code-reviewer"`)     |
| `display-name` | `string`               | Human-readable display name                    |
| `description`  | `string`               | Description of what the agent does             |
| `version`      | `string`               | Semver version of the agent module             |
| `skills`       | `list<skill>`          | Skills the agent provides                      |
| `tools`        | `list<tool-definition>`| Tools the agent exposes                        |

### `chat-request` (record)

Request sent to a guest agent's `chat` function.

| Field             | Type             | Description                     |
|:------------------|:-----------------|:--------------------------------|
| `messages`        | `list<message>`  | The conversation history        |
| `conversation-id` | `string`         | Unique identifier for this conversation |

### `chat-response` (record)

Response returned from a guest agent's `chat` function.

| Field        | Type                  | Description                                  |
|:-------------|:----------------------|:---------------------------------------------|
| `content`    | `string`              | The agent's reply text                       |
| `tool-calls` | `list<tool-call>`     | Tool calls the agent wants the host to run   |
| `usage`      | `option<token-usage>` | Token usage for this response, if tracked    |

---

## Host Imports

These interfaces are provided by chatty to every WASM module. They are the **only** host capabilities available — this keeps the trust surface minimal.

### `llm` — LLM Completion

```wit
interface llm {
    use types.{message, completion-response};
    complete: func(model: string, messages: list<message>, tools: option<string>) -> result<completion-response, string>;
}
```

Call the host's LLM to generate completions. The host manages API keys, rate limiting, and model routing: the request goes through the same provider client the calling agent uses (`chatty_core::services::plugin_llm::PluginLlmProvider`), so OpenRouter, Ollama and Azure OpenAI (API key or Entra ID) all work.

**Parameters**:
- `model` — Empty (`""`) for the calling agent's model, which is what most modules want. Otherwise a model identifier (e.g. `"claude-sonnet-4-20250514"`) or model id that must match a model configured in the host; any other name is refused with an error and no request is sent.
- `messages` — Conversation history to send to the LLM.
- `tools` — Optional JSON-encoded array of tool definitions for the LLM to use: the flat form (`name`, `description`, `parameters`) or the OpenAI wrapped form (`{"type": "function", "function": {...}}`). Pass `none` if the module doesn't need tool use in this completion.

**Returns**: `result<completion-response, string>` — The completion or an error message. `usage.input-tokens` is the whole prompt (cached tokens included). The call is bounded by the module's per-call deadline: past it the host stops waiting and returns `deadline exceeded`.

**Example** (pseudocode):
```
// Simple completion without tools
let messages = [
    { role: system, content: "You are a helpful code reviewer." },
    { role: user, content: "Review this function: fn add(a: i32, b: i32) -> i32 { a + b }" },
];
let response = llm::complete("claude-sonnet-4-20250514", messages, none);
// response.content = "The function looks correct..."
```

**Example with tools** (pseudocode):
```
let tools = some("[{\"name\": \"search\", \"description\": \"Search code\", \"parameters\": {\"type\": \"object\", \"properties\": {\"query\": {\"type\": \"string\"}}}}]");
let response = llm::complete("gpt-4o", messages, tools);
// response.tool-calls may contain: [{ id: "tc_1", name: "search", arguments: "{\"query\": \"error handling\"}" }]
```

### `config` — Configuration

```wit
interface config {
    get: func(key: string) -> option<string>;
}
```

Read the module's configuration: the string → string `[config]` table of its `module.toml`. A non-string value there is a manifest error.

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

Read a file under the module's **file root**: the directory its `module.toml` grants with

```toml
[files]
root = "weights"   # relative to the module directory
```

The root is host-set (the runtime's `ModuleManifest::with_weights_root`), never a `[config]` key: a config value named `weights_root` grants nothing. A module without `[files]` can read no files. Free modules never need this; ML inference modules use it to load weights once at startup.

**Parameters**:
- `path` — Relative to the file root. `/` and `\` both separate components.

**Returns**: the file's bytes, or an error when:
- the module has no file root;
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

Emit log messages that appear in the host's log output.

**Parameters**:
- `level` — Log level: `"trace"`, `"debug"`, `"info"`, `"warn"`, or `"error"`.
- `message` — The log message.

**Example** (pseudocode):
```
logging::log("info", "Starting code review...");
logging::log("debug", "Analyzing 42 files");
logging::log("error", "Failed to parse input: unexpected token");
```

### `billing` — Paid Module Sessions (optional)

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

Only paid modules import this; free modules never call it (zero overhead). A
paid module calls `acquire-session` before doing work — the host asks Hive to
reserve credits and returns a signed session token — then `report-usage` once
the work is done, so Hive can settle the reservation against actual usage.

**`session-info.token`** is a Hive-signed JWT. The current `hive-billing-sdk`
verifies it with **HS256 (HMAC-SHA256) against a shared secret embedded in the
module at compile time**, not an embedded *public* key: there is no Ed25519
verification today, only a documented future upgrade path
(`crates/hive-billing-sdk/src/lib.rs`). HMAC-in-WASM is deterrence, not proof
— a determined attacker can extract the secret from the compiled module — and
the crate's own doc comment says so; high-trust billing should run on Hive's
Firecracker infrastructure, where verification happens server-side instead.

**Parameters** (`acquire-session`):
- `estimated-tokens` — estimated token usage for this invocation.

**Parameters** (`report-usage`):
- `input-tokens` / `output-tokens` — actual tokens consumed.

---

## Guest Exports

Every chatty WASM module must export the `agent` interface.

### `agent` — Agent Interface

```wit
interface agent {
    use types.{chat-request, chat-response, tool-definition, agent-card};
    chat: func(req: chat-request) -> result<chat-response, string>;
    invoke-tool: func(name: string, args: string) -> result<string, string>;
    list-tools: func() -> list<tool-definition>;
    get-agent-card: func() -> agent-card;
}
```

#### `chat`

Handle a chat request and return a response. This is the main entry point for conversational interaction.

**Parameters**:
- `req` — A `chat-request` containing the conversation history and conversation ID.

**Returns**: `result<chat-response, string>` — The response or an error message.

**Example** (pseudocode):
```
// Module receives a chat request
let req = {
    messages: [
        { role: user, content: "Review this PR" },
    ],
    conversation-id: "conv-abc-123",
};

// Module can call host LLM
let llm_response = llm::complete("claude-sonnet-4-20250514", req.messages, none);

// Return response
return ok({
    content: llm_response.content,
    tool-calls: [],
    usage: llm_response.usage,
});
```

#### `invoke-tool`

Invoke a tool exposed by this module. The host calls this when an LLM response includes a tool call matching one of this module's tools.

**Parameters**:
- `name` — Tool name (must match a name from `list-tools`).
- `args` — JSON-encoded arguments matching the tool's `parameters-schema`.

**Returns**: `result<string, string>` — JSON-encoded tool output, or an error message.

**Example** (pseudocode):
```
// Host calls: invoke-tool("search-code", "{\"query\": \"TODO\", \"language\": \"rust\"}")
//
// Module executes the tool logic and returns:
// ok("{\"results\": [{\"file\": \"main.rs\", \"line\": 42, \"text\": \"// TODO: fix this\"}]}")
//
// On error:
// err("Unknown tool: nonexistent-tool")
```

#### `list-tools`

List all tools this module provides. Called by the host during module initialization.

**Returns**: `list<tool-definition>` — All tool definitions.

**Example** (pseudocode):
```
return [
    {
        name: "search-code",
        description: "Search for code patterns across the project",
        parameters-schema: "{\"type\": \"object\", \"properties\": {\"query\": {\"type\": \"string\", \"description\": \"Search query\"}, \"language\": {\"type\": \"string\", \"description\": \"Filter by language\"}}, \"required\": [\"query\"]}",
    },
    {
        name: "run-tests",
        description: "Run the project's test suite",
        parameters-schema: "{\"type\": \"object\", \"properties\": {\"filter\": {\"type\": \"string\", \"description\": \"Test name filter\"}}}",
    },
];
```

#### `get-agent-card`

Return the agent's metadata card. Called by the host during module discovery.

**Returns**: `agent-card` — The module's metadata.

**Example** (pseudocode):
```
return {
    name: "code-reviewer",
    display-name: "Code Reviewer",
    description: "Reviews code changes and suggests improvements",
    version: "1.0.0",
    skills: [
        {
            name: "review-pr",
            description: "Review a pull request for issues and improvements",
            examples: ["Review this PR", "Check my code changes"],
        },
    ],
    tools: [
        {
            name: "search-code",
            description: "Search for code patterns",
            parameters-schema: "{\"type\": \"object\", \"properties\": {\"query\": {\"type\": \"string\"}}, \"required\": [\"query\"]}",
        },
    ],
};
```

---

## World

```wit
world module {
    import llm;
    import config;
    import logging;
    import file;      // optional: ML modules use this to load weights
    import billing;   // optional: only called by paid modules
    export agent;
}
```

The `module` world is the compilation target for all chatty WASM modules. It wires together the host imports and the one guest export.

---

## Resource Limits

Every call into a guest export (`chat`, `invoke-tool`, `list-tools`, `get-agent-card`)
runs inside a sandboxed Wasmtime instance under per-call limits
(`crates/chatty-wasm-runtime/src/limits.rs`, `ResourceLimits`). Fuel and the wall-clock
deadline are reset before each call, so a long-lived module never runs out of a
lifetime budget. The defaults below are also the host ceilings: a module manifest's
`[resources]` section may only lower a limit, never raise it — a larger value is
clamped down to the ceiling.

| Limit | Default = ceiling | Enforcement | Error |
|:------|:------------------|:------------|:------|
| **Fuel** | 10¹² units per call | Wasmtime fuel (≈1 unit per Wasm instruction) | `fuel exhausted` |
| **Wall clock** | 60 s per call, host time included | Epoch interruption (10 ms ticks); host imports (`llm`, `file`, `billing`) stop waiting at the deadline | `deadline exceeded` |
| **Memory** | 256 MiB | Store memory limiter | `memory limit` |
| **Output** | 1 MiB per call | Size of each export's return value | `output too large` |

`list-tools` and `get-agent-card` get a fixed 1 s wall-clock budget regardless of the
call's own `max_execution_ms`.

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

## Versioning Strategy

The WIT package uses [semantic versioning](https://semver.org/): `chatty:module@MAJOR.MINOR.PATCH`.

### Compatibility Rules

| Change Type               | Version Bump | Backward Compatible? |
|:--------------------------|:-------------|:---------------------|
| Add optional field to a record (via new record version) | Minor | Yes — old modules ignore it |
| Add new function to an interface | Minor | Yes — host checks capability |
| Add new interface to world imports | Minor | Yes — modules don't have to use it |
| Remove or rename a field  | **Major**    | **No** — breaks existing modules |
| Remove or rename a function | **Major**  | **No** — breaks existing modules |
| Change a function signature | **Major**  | **No** — breaks existing modules |
| Add new enum variant       | **Major**   | **No** — breaks exhaustive matches |
| Add required export interface | **Major** | **No** — breaks existing modules |

### Evolution Guidelines

1. **Additive changes only** in minor versions. New optional host imports (`file` and `billing` are the two shipped so far) can be added without breaking existing modules since a module that does not import them is unaffected. `http`, `fs` and `process` are hypothetical future examples of the same pattern, not imports that exist today.

2. **New record fields** require creating a new record type (e.g. `chat-request-v2`) because WIT records are structurally typed — adding a field changes the ABI. The old type must be kept for backward compatibility.

3. **New enum variants** are breaking because guest modules may use exhaustive matches. If a new role is needed, bump the major version.

4. **Deprecation flow**: Mark functions/types as deprecated in comments for one minor version before removing in the next major version.

5. **Single live version**: The host registers exactly one WIT package version in the linker — there are no adapter layers for older packages, and a module targeting a superseded version fails to instantiate. Bumping the package version means rebuilding every module against it.

### Current Version: `0.2.0`

Adds the optional `billing` and `file` interfaces over `0.1.0`, which is no longer loadable. The `0.x` series allows breaking changes in minor versions while the interface is being stabilized. Once `1.0.0` is released, the compatibility rules above apply strictly.
