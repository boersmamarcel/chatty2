# System overview

**When to read this:** You need a one-page mental model of Chatty before diving into
crate-level or file-level docs.

Chatty is a Rust agent framework with two user-facing frontends (desktop GPUI and
terminal TUI) sharing one UI-agnostic core. Optional WASM modules and an HTTP
gateway extend the same tool and conversation model to external protocols.

## Three layers

```mermaid
flowchart TB
  subgraph frontends [Frontends]
    GPUI["chatty-gpui<br/>Desktop app"]
    TUI["chatty-tui<br/>Terminal + headless"]
  end

  subgraph core [chatty-core — shared brain]
    Models["Models & stores"]
    Services["Services"]
    Tools["~60 LLM tools"]
    Factories["AgentFactory"]
    Repos["Repositories"]
  end

  subgraph extensions [Extensions]
    WASM["WASM modules<br/>chatty-module-*"]
    Gateway["protocol-gateway<br/>OpenAI / MCP / A2A"]
    Research["Research crates<br/>trace / playbook / flow / optimize"]
  end

  subgraph external [External]
    LLM["LLM providers"]
    MCP["MCP servers"]
    Docker["Docker sandbox"]
    Marketing["Marketing site<br/>github.com/boersmamarcel/chatty"]
    Docs["Developer docs<br/>GitHub Pages mdBook"]
  end

  GPUI --> core
  TUI --> core
  Gateway --> core
  WASM --> core
  Research --> core
  Factories --> LLM
  Tools --> MCP
  Tools --> Docker
  GPUI -.-> Marketing
  GPUI -.-> Docs
```

## Role of each major crate

| Crate | Role in the system |
|-------|-------------------|
| **chatty-core** | Single source of business logic: conversations, tools, LLM agents, settings persistence, sandbox, MCP, memory |
| **chatty-gpui** | Desktop shell: renders UI, owns `StreamManager`, routes entity events through `ChattyApp` |
| **chatty-tui** | Terminal shell + headless/pipe mode for scripting and sub-agents |
| **chatty-wasm-runtime** | Wasmtime host for agent modules (`wasm32-wasip2`) |
| **chatty-module-registry** | Discovers, validates, and loads WASM module manifests |
| **chatty-protocol-gateway** | HTTP façade so external clients can call modules via standard APIs |
| **chatty-module-sdk** | Authoring SDK for third-party WASM agents |
| **chatty-trace / playbook / flow / optimize** | Research crates (self-improvement papers); see `RESERVED.md` |
| **hive-client / hive-billing-sdk** | Hive registry and billing integration |

Dependency rule: **frontends → core**. Core never depends on GPUI or Ratatui.

## End-to-end message path

```mermaid
sequenceDiagram
  participant User
  participant UI as GPUI or TUI
  participant App as ChattyApp / ChatEngine
  participant SM as StreamManager
  participant AF as AgentFactory
  participant LLM as LLM provider
  participant Tools as Native + MCP tools

  User->>UI: Type message
  UI->>App: Submit event
  App->>AF: Build AgentClient
  App->>SM: Register stream
  App->>LLM: stream_prompt (multi-turn)
  loop ReAct loop
    LLM->>Tools: Tool call
    Tools-->>LLM: Tool result
  end
  LLM-->>SM: Stream chunks
  SM-->>UI: StreamManagerEvent
  UI-->>User: Render response
  App->>App: Persist conversation
```

## Where to go next

| Question | Document |
|----------|----------|
| Crate/module layout | [architecture-overview.md](architecture-overview.md) |
| Component relationships & diagrams | [component-map.md](component-map.md) |
| Entity events between GPUI components | [entity-communication.md](entity-communication.md) |
| LLM stream lifecycle | [stream-manager.md](stream-manager.md) |
| Agent quick-start | [AGENTS.md](../AGENTS.md) |
