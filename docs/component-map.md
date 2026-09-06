# Component map

**When to read this:** You need to see how runtime pieces connect — which entity owns
what, where events flow, and which module to open for a given concern.

## Workspace crate relationships

```mermaid
flowchart LR
  subgraph ui [UI layer]
    GPUI[chatty-gpui]
    TUI[chatty-tui]
  end

  subgraph core [chatty-core]
    direction TB
    CModels[models/]
    CServices[services/]
    CTools[tools/]
    CFactory[factories/agent_factory]
    CSettings[settings/]
    CRepos[repositories/]
    CSandbox[sandbox/]
  end

  subgraph wasm [WASM stack]
    Reg[module-registry]
    RT[wasm-runtime]
    GW[protocol-gateway]
    SDK[module-sdk]
  end

  GPUI --> core
  TUI --> core
  GPUI --> Reg
  GPUI --> GW
  GW --> Reg
  GW --> RT
  Reg --> RT
  SDK -.-> RT

  CFactory --> CTools
  CFactory --> CServices
  CServices --> CSandbox
  CSettings --> CRepos
  CModels --> CRepos
```

## chatty-core internal modules

Each box is a top-level module under `crates/chatty-core/src/`.

```mermaid
flowchart TB
  subgraph core [chatty-core]
    models["models/<br/>Conversation, Message,<br/>ConversationsStore, approvals"]
    settings["settings/<br/>ProviderModel, ModelsModel,<br/>McpStore, JSON repos"]
    repositories["repositories/<br/>SQLite conversations"]
    factories["factories/<br/>AgentFactory per provider"]
    services["services/<br/>LLM, shell, MCP, math,<br/>browser (CDP), A2A client, sync"]
    tools["tools/<br/>LLM Tool impls"]
    sandbox["sandbox/<br/>Docker + Monty"]
    token_budget["token_budget/<br/>count, summarize, cache"]
    exporters["exporters/<br/>ATIF, JSONL"]
    auth["auth/<br/>Azure OAuth"]
  end

  factories --> tools
  factories --> services
  services --> sandbox
  services --> settings
  models --> repositories
  settings --> repositories
  token_budget --> models
  exporters --> models
```

| Module | Responsibility | Does NOT contain |
|--------|----------------|------------------|
| `models/` | Pure data + in-memory stores | UI, disk I/O |
| `settings/` | Config models + JSON repositories | GPUI views |
| `repositories/` | SQLite + abstraction traits | Business rules |
| `factories/` | Build `AgentClient`, register tools | Stream UI updates |
| `services/` | Long-running / stateful logic | GPUI `Render` |
| `tools/` | LLM-callable tool definitions | Agent loop itself |
| `sandbox/` | Code execution isolation | Tool approval UI |
| `token_budget/` | Context window accounting | Chat rendering |

## GPUI desktop architecture

`ChattyApp` is the hub; views emit events upward; services live in core.

```mermaid
flowchart TB
  subgraph views [chatty-gpui views]
    Sidebar[SidebarView]
    ChatView[ChatView]
    ChatInput[ChatInput]
    Settings[Settings pages]
  end

  subgraph controller [app_controller]
    ChattyApp[ChattyApp entity]
    MsgOps[message_ops]
    ConvOps[conversation_ops]
    Slash[slash_commands]
  end

  subgraph gpui_models [GPUI-specific models]
    StreamMgr[StreamManager]
    ErrorNotif[ErrorNotifier]
  end

  subgraph core [chatty-core]
    Stores[Global stores]
    AgentFactory[AgentFactory]
    ConvRepo[ConversationRepository]
  end

  Sidebar -->|SidebarEvent| ChattyApp
  ChatInput -->|InputEvent| ChattyApp
  ChatView -->|ChatViewEvent| ChattyApp
  ChattyApp --> MsgOps
  ChattyApp --> ConvOps
  ChattyApp --> Slash
  MsgOps --> AgentFactory
  MsgOps --> StreamMgr
  StreamMgr -->|StreamManagerEvent| ChattyApp
  ChattyApp --> ChatView
  ChattyApp --> Stores
  ConvOps --> ConvRepo
  Settings --> Stores
```

### Entity event bus (simplified)

```mermaid
flowchart LR
  Sidebar -->|SelectConversation| App[ChattyApp]
  Input[ChatInput] -->|PressEnter| App
  App -->|append_text| ChatView
  StreamMgr[StreamManager] -->|TextChunk| App
  App -->|append_text| ChatView
  StreamMgr -->|StreamEnded| App
  App -->|finalize| ChatView
  App -->|refresh list| Sidebar
```

Full topology: [entity-communication.md](entity-communication.md).

## Stream lifecycle ownership

```mermaid
stateDiagram-v2
  [*] --> Active: StreamManager.register
  Active --> Active: TextChunk / ToolCall events
  Active --> Completed: LLM done
  Active --> Cancelled: user stop / cancel_flag
  Active --> Error: stream error
  Completed --> [*]: ChattyApp finalizes + saves
  Cancelled --> [*]: ChattyApp finalizes partial
  Error --> [*]: ErrorNotifier + UI message
```

The async stream loop **only** updates `Conversation` and `StreamManager` — never
GPUI views directly. See [stream-manager.md](stream-manager.md).

## Tool execution path

```mermaid
flowchart TB
  LLM[LLM tool_call] --> Rig[Rig Agent loop]
  Rig --> ToolImpl[chatty-core Tool]
  ToolImpl --> Approval{Needs approval?}
  Approval -->|yes| ApprovalStore[ExecutionApprovalStore]
  ApprovalStore --> UI[GPUI prompt / TUI y/n]
  UI --> ToolImpl
  Approval -->|no| Execute[Run operation]
  Execute --> Shell[shell_service]
  Execute --> FS[filesystem tools]
  Execute --> MCP[McpService]
  Execute --> Sandbox[sandbox / execute_code]
  Execute --> Result[Tool output to LLM]
```

## WASM module stack

```mermaid
flowchart LR
  Author[Module author] --> SDK[chatty-module-sdk]
  SDK --> WASM[".wasm artifact"]
  WASM --> Registry[module-registry]
  Registry --> Runtime[wasm-runtime]
  Runtime --> Tools[invoke_agent / list_agents]
  Gateway[protocol-gateway] --> Runtime
  External[External HTTP client] --> Gateway
  Runtime --> LLM[Host llm::complete import]
  LLM --> Providers[LLM providers]
```

See [build-wasm-module guide](../docs-site/src/dev/guides/build-wasm-module.md)
(sequence diagrams) and [a2a-and-wasm-modules.md](a2a-and-wasm-modules.md).

## Research touchpoints

Production components above are where promoted research mechanisms land. For a
bidirectional map (memory → M4 ACE, token budget → M3/M4, sub-agents → M2 AFlow, …)
see [research/app-research-bridge.md](research/app-research-bridge.md).
