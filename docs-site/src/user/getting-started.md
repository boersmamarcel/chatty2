# Getting started

**When to read this:** You are setting up Chatty for the first time.

Product overview: [Why Chatty?](./overview.md). Demos and marketing:
[boersmamarcel/chatty](https://github.com/boersmamarcel/chatty).

## 1. Download

Grab the latest release from [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases):

| Platform | Format |
|----------|--------|
| macOS (Intel & Apple Silicon) | `.dmg` installer |
| Linux (x86_64) | `.tar.gz` archive |
| Windows (x86_64) | `.exe` installer |

## 2. Add a provider

On first launch, connect at least one LLM provider.

1. Click the **gear icon** in the title bar to open Settings
2. Open the **Providers** tab
3. **Add Provider** and pick one (OpenRouter, Ollama, Azure OpenAI, …)
4. Paste your API key (Ollama needs none — it connects to your local instance)

A recorded walkthrough lives in the repo
([`add_provider_and_model.gif`](https://github.com/boersmamarcel/chatty2/blob/main/assets/animations/add_provider_and_model.gif);
~50 MB, not inlined here). Capability details:
[Providers & models](./providers-and-models.md).

## 3. Add a model

1. Still in Settings, open the **Models** tab
2. **Add Model**
3. Pick a provider and enter a model ID (e.g. `gpt-4o`, `claude-sonnet-4-20250514`, `gemini-2.0-flash`)

Chatty auto-detects vision and PDF support. No extra capability flags to set.

## 4. Start chatting

Close Settings and send a message. A new conversation shows a start screen of
active capabilities — skills, MCP servers, agents, file access, web tools,
memory, and workspace status — before you type anything. Switch models with
the selector at the bottom of the chat.

- Type `/` for the slash-command picker (`↑/↓`, `Enter`). Commands include
  `/clear`, `/new`, `/compact`, `/context`, `/copy`, `/cwd`, `/cd`, `/add-dir`,
  and `/agent`. Workspace skills (`.claude/skills/`) and global skills also
  appear with a `[skill]` badge.
- Type `@` for a file picker over the current working directory. Hidden files
  and common build dirs (`.git`, `node_modules`, `target`) are excluded.

Full command list: [slash commands reference](../dev/reference/slash-commands.md).

## 5. Enable agentic tools

Filesystem, sandboxed shell, MCP, and sub-agents are **off by default**. Enable
them in **Settings → Code Execution**:

1. Set a **workspace directory** (absolute path) — tools can only touch files inside it
2. Toggle **code execution** on
3. Choose an **approval mode**:
   - **Ask every time** — approve each tool call (recommended at first)
   - **Auto-approve** — tools run without prompting
   - **Deny all** — tools are listed but blocked

**Per-chat working directory:** the folder icon in the chat input bar opens an
OS directory picker and overrides the global workspace for that conversation.
`×` resets to the global default. The override is saved with the conversation.

**Code execution:** simple stdlib Python runs on the host via MontySandbox
(~5–50 ms). For JavaScript, TypeScript, Rust, Bash, or third-party Python
packages, enable **Docker Fallback** (Docker must be running). Chatty probes
common socket paths, including rootless Docker and Docker Desktop. A custom
**Docker Host** field covers non-standard sockets
(e.g. `/run/user/1000/docker.sock`).

Isolation details: [Security & sandboxing](./security.md). Tool list:
[Agentic tools](./agentic-tools.md).

## Desktop vs terminal

| App | Use when |
|-----|----------|
| `chatty` (GPUI) | Daily interactive work, settings UI, attachments |
| `chatty-tui` | Terminal, scripting, headless sub-agents |

Install `chatty-tui` from the desktop app (macOS: **Chatty → Install CLI**;
Linux/Windows: **Settings → General → Install CLI…**) or from the same release
package. Zero-config Ollama / OpenAI-compatible servers:
[Terminal interface](./terminal.md).

## Next

- [Why Chatty?](./overview.md)
- [Agents](./agents.md)
- [Providers & models](./providers-and-models.md)
- [Agentic tools](./agentic-tools.md)
