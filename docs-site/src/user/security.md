# Security & sandboxing

**When to read this:** How Chatty constrains tool access. Enable tools in
[Getting started](./getting-started.md) or [Agentic tools](./agentic-tools.md).

## Workspace sandboxing

Filesystem and bash tools are scoped to the workspace directory you configure.
The agent cannot read or write outside that boundary. `/add-dir` and the
per-chat folder picker widen that boundary explicitly.

## Shell sandboxing

| Platform | Mechanism |
|----------|-----------|
| Linux | [bubblewrap](https://github.com/containers/bubblewrap) — separate process, network, and mount namespaces |
| macOS | `sandbox-exec` — blocks `.ssh`, `.aws`, `.gnupg`, and other sensitive paths |

Optional **network isolation** blocks shell commands from making network
requests at all.

## Code execution isolation

| Path | When |
|------|------|
| **MontySandbox** | Simple stdlib Python on the host (~5–50 ms), memory cap, stripped environment |
| **Docker fallback** | JS/TS/Rust/Bash, or Python that needs third-party packages |

Docker containers are isolated from the host filesystem and network.
MontySandbox falls back to Docker on import errors or unsupported syntax when
**Docker Fallback** is on. Configure both in **Settings → Code Execution**.

## Approval flows

Side-effect tools (file writes, shell, sub-agents) honor three modes:

| Mode | Behavior |
|------|----------|
| **Ask every time** | Prompt before each side effect; you see the exact command or file path |
| **Auto-approve sandboxed** | Shell commands run when sandboxed without prompting; **workspace file writes apply immediately** (Cursor / Claude Code style) |
| **Auto-approve all** | All side effects run without prompting |

File writes inside the configured workspace directory are auto-approved under **Auto-approve sandboxed** and **Auto-approve all**. Use **Ask every time** if you want to confirm every `write_file`, edit, or diff before it lands.

## Secrets & key masking

- **MCP API keys** — the agent may see `has_api_key: true`, never the value
- **User secrets** — Settings → Secrets inject into shell sessions; values are not logged or shown in tool output
- **No product telemetry or hosted relay** — traffic goes only to providers, MCP/A2A services, websites, package registries, and update endpoints you configure or invoke

When a surface sends MCP config to the model it must use `masked_env()`, not
raw `.env`. Sending `****` back from the model means “keep the stored value.”

## Related

- [Agentic tools](./agentic-tools.md)
- [Sub-agents](./sub-agents.md)
