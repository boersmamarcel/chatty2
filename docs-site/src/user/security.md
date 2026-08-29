# Security & sandboxing

**When to read this:** Understand how Chatty constrains agent tool access.

## Workspace sandboxing

Filesystem and bash tools are scoped to the workspace directory you configure. The agent cannot read or write outside that boundary.

## Shell sandboxing

| Platform | Mechanism |
|----------|-----------|
| Linux | [bubblewrap](https://github.com/containers/bubblewrap) — separate process, network, and mount namespaces |
| macOS | `sandbox-exec` — blocks `.ssh`, `.aws`, `.gnupg`, and other sensitive paths |

Optional **network isolation** blocks shell commands from making network requests.

## Code execution isolation

| Path | When |
|------|------|
| **MontySandbox** | Simple stdlib Python on host (~5–50 ms) |
| **Docker fallback** | JS/TS/Rust/Bash, or Python with third-party packages |

Docker containers are fully isolated from host filesystem and network. MontySandbox runs with a memory cap and stripped environment, falling back to Docker on import errors.

Configure in **Settings → Code Execution**.

## Approval flows

Side-effect tools (writes, shell, sub-agents) support three modes:

| Mode | Behavior |
|------|----------|
| **Ask every time** | Prompt before each tool call |
| **Auto-approve** | Run immediately — trusted workflows |
| **Deny all** | Tools visible but blocked |

## Secrets & key masking

- **MCP API keys** — agent sees `has_api_key: true` but never the value
- **User secrets** — Settings → Secrets inject into shell sessions; values never revealed in tool output or logs
- **No product telemetry** — network traffic goes only to providers, MCP/A2A services, and endpoints you configure

## Related

- [Agentic tools](./agentic-tools.md)
- [Sub-agents](./sub-agents.md)
