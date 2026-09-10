# Terminal interface

**When to read this:** You want to use Chatty from a terminal — interactively, as a one-shot command, or in a pipeline — with the same providers and models as the desktop app.

## Install

| Method | How |
|--------|-----|
| **From the desktop app** | macOS: **Chatty** menu → **Install CLI…**. Linux and Windows: **Settings → General → Install CLI…**. The same spot offers **Reinstall CLI** later |
| **From the release** | The desktop package already contains `chatty-tui` |
| **From source** | Build it yourself: [Build and run](../dev/start/build-and-run.md) |

Where it lands: macOS puts a link in `/usr/local/bin` (you may be asked for an administrator password); Linux copies the binary to `~/.local/bin` — add that folder to your `PATH` if `chatty-tui` is not found; Windows adds the app's install folder to your user `PATH`, so open a new terminal afterwards.

> [!NOTE]
> After a desktop update on Linux, the copy in `~/.local/bin` is refreshed the next time the desktop app launches. `/update` inside `chatty-tui` does the same on demand.

## Modes

| Mode | Command | Use |
|------|---------|-----|
| **Interactive** | `chatty-tui` | Full-screen chat with a model picker, tool picker and approval prompts |
| **Headless** | `chatty-tui --headless -m "question"` | One message in, the answer on stdout — for scripts and [sub-agents](./sub-agents.md) |
| **Pipe** | `cat notes.md \| chatty-tui --pipe` | stdin is the message; the answer goes to stdout |

Useful flags: `--model <id or name>` picks a model (exact id, then name, then substring); `--enable` / `--disable` switch tool groups for this run (`shell`, `fs-read`, `fs-write`, `fetch`, `git`, `code-exec`, `docker-exec`); `--auto-approve` skips every approval prompt ([Security & sandboxing](./security.md)). Full list: [CLI flags](../dev/reference/cli-flags.md).

## Zero-config quick start

Talk to a running model server without opening the desktop app or storing a key:

```bash
# Ollama (discovers models at localhost:11434; pass a URL for another host)
chatty-tui --ollama
chatty-tui --ollama --model llama3.2

# vLLM, llama.cpp, LM Studio or any OpenAI-compatible server
chatty-tui --openai-compat-url http://localhost:8000
chatty-tui --openai-compat-url http://localhost:8000 --model my-model --api-key sk-...
```

## Welcome screen and status bar

An empty interactive session shows what is active: model and context window, workspace and git branch, enabled tool groups, internet capabilities (fetch, search, browser, cloud sandbox, MCP) and runtime features (memory, modules, remote agents). MCP, memory and embeddings load in the background — badges show `⟳` (for example `[MCP ⟳]`) and the status bar reads *loading services…* until they are ready.

The status bar always shows the app version, the working directory and the git branch when you are inside a repository, plus the branch's pull request (`#591 open ✓`) when git integration is on. Footer hints switch to `Ctrl+C stop` while a reply streams. A scrollbar appears when the transcript overflows; scrolling up unpins auto-scroll, `End` re-pins it.

Tool calls in the transcript are folded by default — the tool and its main argument on one line, the result trimmed to a few lines with a `… +N lines` marker. Errors are always shown in full. Press `Ctrl+R` or run `/verbose` to switch to the full, untrimmed input/output for every tool call; the footer hint shows which mode is active.

## Keys

| Key | Action |
|-----|--------|
| `Enter` | Send |
| `/` | Command picker (`↑/↓`, `Tab` or `Enter`) |
| `@` | File picker (type to filter) |
| `PageUp` / `PageDown`, `Shift+↑/↓`, mouse wheel | Scroll |
| `End` | Jump to the bottom and resume auto-scroll |
| `y` / `n` | Approve or deny a tool prompt |
| `1`-`9` | Pick an option when the agent asks a clarifying question (`ask_user`) |
| `t` | Type a custom answer instead of picking an option |
| `Ctrl+C` | Stop streaming, or quit when idle |
| `Ctrl+Q` | Quit immediately |
| `Ctrl+R` / `/verbose` | Toggle full tool-call payloads on or off |

A pending clarifying question replaces the input row and takes over the
keyboard until answered — `Ctrl+C`/`Ctrl+Q` still work. Multiple questions are
answered one at a time; press `Esc` while typing a custom answer to go back
to the options.

The terminal app has a few commands of its own — `/model`, `/tools`, `/modules`, `/update`, `/quit` — alongside the shared ones. All of them: [slash commands](../dev/reference/slash-commands.md).

`/online` shows where the current conversation runs and, before anything moves, a table of what a move would and would not carry. `/online <server-url>` uploads the conversation's history to that `chatty-server` and continues it there; `/online off` brings it back to this machine. Workspace files, attachments, MCP servers, memory, skills and provider API keys never leave this machine.

## Shared configuration

`chatty-tui` reads the same settings as the desktop app — providers, models, tools, secrets and memory — so run the desktop app once to set things up, or skip that entirely with `--ollama` / `--openai-compat-url`. Where the files live: [Advanced](./advanced.md).

## Next

- [Sub-agents](./sub-agents.md)
- [Getting started](./getting-started.md)
- [Advanced](./advanced.md)
