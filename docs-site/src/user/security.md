# Security & sandboxing

**When to read this:** You want to know what the agent can and cannot touch, what each approval mode does, and how secrets are handled. Switching tools on is covered in [Agents & tools](./agents-and-tools.md).

## The workspace boundary

File, shell and git tools are confined to the **Workspace Directory** you set in Settings → Code Execution. The agent cannot read or write outside it. The per-chat folder picker and `/add-dir` widen that boundary explicitly, per conversation, and nothing else does.

## Shell sandbox

On Linux and macOS, shell commands run in an isolated process that sees the workspace but not sensitive folders such as `.ssh`, `.aws` and `.gnupg`. On Linux the sandbox needs the `bubblewrap` package from your distribution; if it is missing, commands still run but count as unsandboxed. Windows has no shell sandbox, so commands there are always unsandboxed — which matters for the approval modes below.

Two more switches on the same page: **Network Isolation** stops sandboxed commands from reaching the network at all, and **Timeout** / **Max Output** cap how long a command may run and how much output it may return.

## Code execution isolation

The code runner has two paths. Simple Python runs on the host in a restricted interpreter with a memory cap and a stripped environment. Everything else — other languages, or Python that needs third-party packages — goes to a fresh Docker container that is isolated from the host filesystem and network, and only when **Enable Docker Fallback** is on. The cloud sandbox on Settings → Internet runs code on a remote service instead of your machine.

## Approval modes

Settings → Code Execution → **Approval Mode** has three settings:

| Mode | What happens |
|------|--------------|
| **Always Ask (Safest)** | Every side effect — file write, shell command, git change, spreadsheet edit — pauses for your approval, and you see the exact command or path first |
| **Auto-approve Sandboxed** (default) | Shell commands run without asking when they are inside the sandbox, and file and spreadsheet writes inside the workspace apply immediately. Unsandboxed commands and git changes still ask |
| **Auto-approve All (Dangerous)** | Everything runs without asking, including unsandboxed commands, git changes and sub-agents' tool calls |

A pending approval waits five minutes; if you do not answer, the call fails and the agent is told so. Reads, searches, queries, web fetches and the browser tools never ask in any mode. The terminal app uses `y` / `n` for the same prompts and `--auto-approve` to skip them.

> [!WARNING]
> **Auto-approve All** lets the model run any enabled tool without review. Use it only for a workspace you can afford to lose, and prefer a per-run `--auto-approve` in the terminal for scripted jobs.

## Secrets

- **Settings → Secrets** holds key–value pairs that are exported as environment variables in every shell session. The agent can use the *names* in scripts (`os.environ["API_KEY"]`) but never sees the values; they are not logged and are masked in tool output.
- **Provider keys** are stored with your app settings, never in your project files, and are not exposed to the model.
- **MCP server keys** are masked when the agent lists your servers: it may learn that a key exists, never the value.
- **No telemetry, no relay.** Chatty sends traffic only to the providers, MCP servers, agents and websites you configure or ask it to use, plus GitHub for update checks. Sign-in tokens for catalog extensions stay on your machine.

## Next

- [Agents & tools](./agents-and-tools.md)
- [Extensions](./extensions.md)
- [Advanced](./advanced.md)
