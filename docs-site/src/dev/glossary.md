# Glossary

**When to read this:** A word in a page, a commit message or a log line means something specific in Chatty and you want the one-paragraph definition plus the page that owns it.

## Conversations and streaming

**Exchange · turn · API call.** An *exchange* is one user text message answered by one assistant text message, whatever tool round-trips sit between them (`services::exchange_count` counts them; the title generator triggers on the first one). A *turn* is everything the agent does to produce that answer, including every tool call it makes. An *API call* is one provider request; a turn with tool calls makes several. Token usage is recorded per API call (`ApiCallUsage`) and aggregated into the turn's `TokenUsage`, which is why a cache hit rate is a per-call property. Owning page: [Token budget](./architecture/token-tracking.md).

**`__pending__` stream.** The key `StreamManager` registers a stream under when the user sends a message before the conversation has an id; once the id exists the stream is promoted to it with `promote_pending`. Owning page: [Stream lifecycle](./architecture/stream-manager.md).

**Preamble.** rig's word for the system prompt. `build_preamble` in `factories/agent_factory/preamble_builder.rs` extends the configured prompt with the tool summary, formatting guide, memory instructions and the names of user secrets; on OpenRouter it is also the first prompt-cache breakpoint. Owning page: [Contributing patterns](./contributing-patterns.md) (token usage section).

**System trace.** The `SystemTrace` JSON persisted next to an assistant message: a sequence of thinking blocks, tool calls, approval prompts and clarification prompts. The transcript renders it; the exporters read it. Owning page: [Debug](./guides/debug.md) (reading a `system_trace`).

**Artifact.** A file the agent produced that the desktop opens in the artifact panel beside the transcript: a chart, a PDF from `compile_typst`, a browser screenshot, a query table. Tools hand the path over through `PendingArtifacts` (the `add_attachment` path), so the model sees it on the next turn rather than inline. Owning pages: [Chatting](../user/chatting.md) for users, [Rendering pipeline](./architecture/rendering-system.md) for the transcript.

## Desktop (GPUI)

**Entity.** A GPUI component with state and lifecycle, held as `Entity<T>`: it implements `Render` and usually `EventEmitter`, mutates itself inside `update`, and calls `cx.notify()` to re-render. Owning page: [Entity communication](./architecture/entity-communication.md).

**Global.** Process-wide state reached with `cx.global::<T>()` / `cx.set_global`. Entities go into globals through `GlobalWeakEntity<T>` (default) or `GlobalStrongEntity<T>` (when the global must keep the entity alive). chatty-core types get their `impl Global` behind the `gpui-globals` feature. Owning page: [Contributing patterns](./contributing-patterns.md).

**Notifier.** An entity that exists only to emit events other entities subscribe to (`ModelsNotifier`, `AgentConfigNotifier`), so a controller can announce "models loaded" without knowing who listens. Owning page: [GPUI event catalog](./reference/event-catalog.md).

## Agent tools and security

**Approval mode.** `ApprovalMode` in the execution settings: `AlwaysAsk` (default), `AutoApproveSandboxed`, `AutoApproveAll`. It decides whether shell commands and file writes stop for a y/n prompt (`ExecutionApprovalStore`, `WriteApprovalStore`); `chatty-tui --auto-approve` forces `AutoApproveAll`. Owning page: [Security & approvals](../user/security.md); fields in [Settings schema](./reference/settings-schema.md).

**Workspace.** The absolute directory in `ExecutionSettingsModel.workspace_dir` that roots the filesystem, shell, git and browser tools; paths outside it are refused. The TUI defaults it to the current directory. Project-local skills live in `<workspace>/.claude/skills/`, browser dumps in `<workspace>/.chatty/browser/`. Owning page: [Agents & tools](../user/agents-and-tools.md).

**Lane A / Lane B (browser).** Two navigation policies for the built-in headless Chrome. Lane A is what ships: an ephemeral profile that reaches `localhost` and workspace-local `file://` URLs, widened to the public web when the internet-access setting is on, always behind the same SSRF denylist as `fetch`. Lane B — a per-task origin allowlist with a persistent, credentialed profile — is designed into `NavigationPolicy` but not implemented. Owning page: [Contributing patterns](./contributing-patterns.md) (Built-in Browser); user view in [Agents & tools](../user/agents-and-tools.md).

**Sub-agent.** What the `sub_agent` tool spawns: a separate `chatty-tui --headless` process with the parent's tool set that works on a delegated task and returns its answer, reporting its turn as `CHATTY_EVENT` lines (serialized `SessionEvent`s) on stderr. Not to be confused with `invoke_agent`, which talks A2A. Owning page: [Sub-agents](../user/sub-agents.md).

**Skill.** A `SKILL.md` in `<workspace>/.claude/skills/<name>/` (project-local) or `<data dir>/chatty/skills/<name>/` (global), loaded by `SkillService`, read with `read_skill`, written with `save_skill`, and offered in the slash-command picker. Owning page: [Memory & skills](../user/memory-and-skills.md); internals in [Agent memory](./architecture/agent-memory.md).

**MCP.** Model Context Protocol: external tool servers configured under Settings → Extensions, started by `McpService`, and attached to the agent as tools. Their environment variables are shown to the model only through `masked_env()`. Owning page: [Extensions & MCP](../user/extensions.md); the shipped list is the [curated MCP catalog](./architecture/curated-mcp-catalog.md).

**A2A.** Agent-to-Agent protocol. Remote agents configured in settings and local WASM modules are both reached with `invoke_agent` over A2A, the latter through the protocol gateway on `localhost:8420`. Owning page: [A2A and WASM modules](./architecture/a2a-and-wasm-modules.md).

**WASM module.** A sandboxed `wasm32-wasip2` component that implements `ModuleExports`, ships with a `module.toml`, runs inside `chatty-wasm-runtime` and is discovered by `chatty-module-registry`. It can call the host LLM but never sees API keys. Owning pages: [Build a WASM plugin](./guides/build-wasm-module.md), [WIT interface](./architecture/wit-reference.md).

## Research

**ATIF.** Agent Trace Interchange Format, the trace export produced by `exporters::atif_exporter` and consumed by `chatty-trace`. Owning pages: [chatty-trace](./crates/chatty-trace.md), [M0 Trace contract](./research/modules/m0-trace.md).

**M0–M4.** The research modules, one per paper, and the crates they land in: M0 trace contract (`chatty-trace`), M1 ReAct (`chatty-core`), M2 AFlow (`chatty-flow`), M3 GEPA (`chatty-optimize`), M4 ACE (`chatty-playbook`). Owning page: [Research modules](./research/modules/index.md).
