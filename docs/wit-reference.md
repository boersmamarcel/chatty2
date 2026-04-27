# WIT Interface Reference

> **Package**: `chatty:module@0.1.0`\
> **Source**: [`wit/chatty-module.wit`](../wit/chatty-module.wit)

This document describes the WIT (WebAssembly Interface Types) contract between chatty (the host) and WASM modules (guests). Every chatty WASM module must target the `module` world defined here.

---

## Architecture Overview

```
┌──────────────────────────────────────────────┐
│                chatty (host)                 │
│                                              │
│  ┌─────────┐  ┌──────────┐  ┌────────────┐  │
│  │   llm   │  │  config  │  │  logging   │  │
│  │ import  │  │  import  │  │  import    │  │
│  └────┬────┘  └────┬─────┘  └─────┬──────┘  │
│       │            │              │          │
├───────┼────────────┼──────────────┼──────────┤
│       ▼            ▼              ▼          │
│  ┌──────────────────────────────────────┐    │
│  │          WASM Module (guest)         │    │
│  │                                      │    │
│  │  exports: agent                      │    │
│  │    • chat(req) → response            │    │
│  │    • invoke-tool(name, args) → result│    │
│  │    • list-tools() → definitions      │    │
│  │    • get-agent-card() → card         │    │
│  └──────────────────────────────────────┘    │
└──────────────────────────────────────────────┘
```

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

Call the host's LLM to generate completions. The host manages API keys, rate limiting, and model routing.

**Parameters**:
- `model` — Model identifier (e.g. `"claude-sonnet-4-20250514"`, `"gpt-4o"`). Must match a model configured in the host.
- `messages` — Conversation history to send to the LLM.
- `tools` — Optional JSON-encoded array of tool definitions for the LLM to use. Pass `none` if the module doesn't need tool use in this completion.

**Returns**: `result<completion-response, string>` — The completion or an error message.

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

Read configuration values set by the user for this module. The host manages the key-value store.

**Parameters**:
- `key` — The configuration key to look up.

**Returns**: `option<string>` — The value, or `none` if the key is not set.

**Example** (pseudocode):
```
let api_key = config::get("api-key");       // some("sk-...")
let threshold = config::get("threshold");    // some("0.8")
let missing = config::get("nonexistent");    // none
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
    export agent;
}
```

The `module` world is the compilation target for all chatty WASM modules. It wires together the three host imports and the one guest export.

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

1. **Additive changes only** in minor versions. New optional host imports (`http`, `fs`, `process`) can be added without breaking existing modules since they simply won't import them.

2. **New record fields** require creating a new record type (e.g. `chat-request-v2`) because WIT records are structurally typed — adding a field changes the ABI. The old type must be kept for backward compatibility.

3. **New enum variants** are breaking because guest modules may use exhaustive matches. If a new role is needed, bump the major version.

4. **Deprecation flow**: Mark functions/types as deprecated in comments for one minor version before removing in the next major version.

5. **Multi-version support**: The host should support loading modules targeting `@0.1.x` even after `@0.2.0` is released, via adapter layers.

### Current Version: `0.1.0`

This is the initial unstable release. The `0.x` series allows breaking changes in minor versions while the interface is being stabilized. Once `1.0.0` is released, the compatibility rules above apply strictly.
