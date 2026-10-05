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

**Workspace.** The absolute directory in `ExecutionSettingsModel.workspace_dir` that roots the filesystem, shell, git and browser tools; paths outside it are refused. The TUI defaults it to the current directory. Project skills live in `<workspace>/.agents/skills/` or `<workspace>/.claude/skills/`, browser dumps in `<workspace>/.chatty/browser/`. Owning page: [Agents & tools](../user/agents-and-tools.md).

**Lane A / Lane B (browser).** Two navigation policies for the built-in headless Chrome. Lane A is what ships: an ephemeral profile that reaches `localhost` and workspace-local `file://` URLs, widened to the public web when the internet-access setting is on, behind the same SSRF denylist as `fetch` unless the workspace's `allow_private_network_access` toggle opts into reaching its own LAN too (AGE-459) — the link-local/cloud-metadata range stays refused regardless. Lane B — a per-task origin allowlist with a persistent, credentialed profile — is designed into `NavigationPolicy` but not implemented. Owning page: [Contributing patterns](./contributing-patterns.md) (Built-in Browser); user view in [Agents & tools](../user/agents-and-tools.md).

**Sub-agent.** A delegated worker: a separate `chatty-tui` process with the parent's tool set that works on one task and reports back. The parent asks for one through `invoke_agent` against the broker's `local-agent`, and the worker reports over the broker's participant socket. Owning page: [Sub-agents](../user/sub-agents.md).

**Broker.** The half of the protocol gateway that serves *local participants* — `chatty-tui` processes the broker spawns on a connection it made for each, and which answer delegated tasks at `/a2a/{name}` under the name the broker gave them (ADR-0011, ADR-0020). The desktop starts it from the module settings; a terminal leader starts its own with `--broker`. Unix only. Owning page: [Agents and specs](./architecture/agents-and-specs.md#local-participants-adr-0011).

**Virtual agent.** A named worker the broker publishes — `local-agent` by default, or each agent spec `module_settings.virtual_agents` / a team's `agents` names: a name, an optional model, a tool profile or disabled groups, a preamble and its own budget. Roles live in specs, never on the `invoke_agent` call.

**Agent spec.** The one declarative definition of an agent (AGE-614): `[agent]`, `[tools]`, `[[plugins]]`, `[swarm]`, `[budget]` in `<name>.toml` under `.chatty/agents/`, the data directory or the presets; `AgentBuildContext::from_spec` builds it. Owning page: [Agents and specs](./architecture/agents-and-specs.md#local-agent--a-chatty-agent-in-its-own-process).

**Tool profile.** A named allowlist of tool *names* (`coordinator`, `coder`, `reviewer` in `tool_profile.rs`) that is a worker's whole tool set, MCP included; it only ever removes tools. Passed as `chatty-tui --tools`. Contrast tool *groups*, which `--enable` / `--disable` switch.

**Team.** A directory `teams/<id>/team.json` + `SKILL.md` declaring a leader (model, profile, preamble), a roster of virtual agents, a verification command, the skill the leader follows and a turn budget; run with `chatty-tui --team <id>`. Two presets ship, both experimental: `crosscheck` and `crosscheck-data`. Owning page: [Sub-agents › Teams](../user/sub-agents.md#teams).

**Evidence envelope.** The runner's — not the model's — account of a worker's output, appended to every delegation reply as a fenced `evidence` block and carried on the terminal status's `metadata.evidence`: branch, base, commit count, diff stat and, when the team declares one, the verification command's exit code and tail. Empty branch, no envelope.

**Skill.** A `SKILL.md` in `.agents/skills/<name>/` or `.claude/skills/<name>/` — in the workspace and its parents up to the git root (project), or under `~` (global) — loaded by `SkillService`, read with `read_skill`, written with `save_skill`, and offered in the slash-command picker. Owning page: [Memory & skills](../user/memory-and-skills.md); internals in [Agent memory](./architecture/agent-memory.md).

**MCP.** Model Context Protocol: external tool servers configured under Settings → Extensions, started by `McpService`, and attached to the agent as tools. Their environment variables are shown to the model only through `masked_env()`. Owning page: [Extensions & MCP](../user/extensions.md); the shipped list is the [curated MCP catalog](./architecture/curated-mcp-catalog.md).

**A2A.** Agent-to-Agent protocol. Remote agents configured in settings and the local agent specs the broker serves are both reached with `invoke_agent` over A2A, the latter through the protocol gateway on `localhost:8420`. Owning page: [Agents and specs](./architecture/agents-and-specs.md).

**WASM module (plugin).** A sandboxed `wasm32-wasip2` component targeting `chatty:plugin@0.3.0` (the SDK's `Plugin` trait), with a `module.toml`, run by `chatty-wasm-runtime` and discovered by `chatty-module-registry`. It contributes tools to the agent specs that list it and is never an agent itself. It can call the host LLM but never sees API keys. Owning pages: [Build a WASM plugin](./guides/build-wasm-module.md), [WIT interface](./architecture/wit-reference.md).

## Research

**ATIF.** Agent Trace Interchange Format, the trace export produced by `exporters::atif_exporter` and consumed by `chatty-trace`. Owning pages: [chatty-trace](./crates/chatty-trace.md), [M0 Trace contract](./research/modules/m0-trace.md).

**M0–M4.** The research modules, one per paper, and the crates they land in: M0 trace contract (`chatty-trace`), M1 ReAct (`chatty-core`), M2 AFlow (`chatty-flow`), M3 GEPA (`chatty-optimize`), M4 ACE (`chatty-playbook`). Owning page: [Research modules](./research/modules/index.md).
