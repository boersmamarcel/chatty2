# Security & sandboxing

How Chatty constrains tool access. Enable tools in
[Agentic tools](./agentic-tools.md).

## Workspace sandbox

Filesystem and shell tools are scoped to the workspace directory you set. The
agent cannot read or write outside that boundary.

## Shell sandbox

| Platform | Mechanism |
|----------|-----------|
| Linux | [bubblewrap](https://github.com/containers/bubblewrap) — separate process, network, and mount namespaces |
| macOS | `sandbox-exec` — blocks `.ssh`, `.aws`, `.gnupg`, and other sensitive paths |

Optional **network isolation** blocks shell commands from making network
requests.

## Code execution

| Path | When |
|------|------|
| **MontySandbox** | Simple stdlib Python on the host (~5–50 ms), memory-capped, stripped environment |
| **Docker fallback** | JS/TS/Rust/Bash, or Python that needs third-party packages |

Docker containers are isolated from the host filesystem and network.
MontySandbox falls back to Docker on import errors when **Docker Fallback** is
on. Configure both in **Settings → Code Execution**. Chatty looks for common
Docker sockets (including rootless and Docker Desktop); set **Docker Host** if
yours is elsewhere.

## Approval flows

Side-effect tools (writes, shell, sub-agents) use one of three modes:

| Mode | Behavior |
|------|----------|
| **Ask every time** | Prompt before each call; you see the command first |
| **Auto-approve** | Run immediately — for workflows you already trust |
| **Deny all** | Tools stay visible to the model but do not execute |

## Secrets and key masking

- **MCP API keys** — the agent sees that a key is set, never the value
- **User secrets** — **Settings → Secrets** injects values into shell
  sessions; they are not logged or shown in tool output
- **No product telemetry** — traffic goes only to providers, MCP/A2A
  services, and endpoints you configure

## Related

- [Agentic tools](./agentic-tools.md)
- [Sub-agents](./sub-agents.md)
