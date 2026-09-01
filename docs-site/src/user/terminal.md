# Terminal interface (`chatty-tui`)

Use Chatty from a terminal — interactive, headless, or piped.

`chatty-tui` reads the same provider and model config as the desktop app.

## Modes

| Mode | Command | Description |
|------|---------|-------------|
| **Interactive** | `chatty-tui` | Full-screen TUI with scrollable chat, model picker, approvals |
| **Headless** | `chatty-tui --headless -m "question"` | One message → stdout (scripts, sub-agents) |
| **Pipe** | `cat file.rs \| chatty-tui --pipe` | Read stdin as the message, print the response |

## Zero-config

Talk to a running model server without opening the desktop app:

```bash
# Ollama (discovers models on localhost:11434)
chatty-tui --ollama
chatty-tui --ollama --model llama3.2

# vLLM / llama.cpp / LM Studio (OpenAI-compatible)
chatty-tui --openai-compat-url http://localhost:8000
chatty-tui --openai-compat-url http://localhost:8000 --model my-model --api-key sk-...
```

## Install

| Method | How |
|--------|-----|
| **Desktop app** | macOS: Chatty menu → Install CLI. Linux/Windows: Settings → General → Install CLI |
| **Releases** | Same archive as the desktop app includes `chatty-tui` |
| **Source** | `cargo install --path crates/chatty-tui` |

The desktop installer copies the binary to `~/.local/bin` on Linux (or your
user bin directory on Windows).

## Welcome screen and status bar

An empty interactive session shows the active model, context window,
workspace, git branch, enabled tools, internet capabilities, and runtime
features (memory, modules, agents). MCP and memory load in the background —
badges show `⟳` until they are ready.

The status bar shows app version, cwd (truncated), and git branch. Footer
hints switch to `Ctrl+C stop` while a response is streaming. Scroll up to
unpin from the bottom; `End` re-pins.

## Keybindings (interactive)

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `/` | Slash-command picker |
| `@` | File picker (type to filter) |
| `PageUp`/`PageDown`, `Shift+↑/↓`, mouse wheel | Scroll |
| `End` | Jump to bottom, resume auto-scroll |
| `y` / `n` | Approve / deny a tool prompt |
| `Ctrl+C` | Stop streaming (or quit if idle) |
| `Ctrl+Q` | Quit immediately |

Launch overrides: `--enable tool1,tool2` / `--disable tool1,tool2`.

## Slash commands (TUI)

| Command | Action |
|---------|--------|
| `/model [query]` | Switch model |
| `/tools [name]` | Toggle tool groups |
| `/modules [show\|enable\|disable\|dir\|port]` | Module runtime settings |
| `/add-dir <dir>` | Expand workspace |
| `/agent <prompt>` | Headless sub-agent |
| `/clear`, `/new` | Fresh conversation |
| `/compact` | Summarize older messages |
| `/context` | Token usage and cwd |
| `/copy` | Copy latest response |
| `/update` | CLI auto-update |
| `/cwd`, `/cd [dir]` | Show or change cwd |
| `/quit`, `/exit` | Quit |

## Config sharing

Config lives under `~/.config/chatty/` (or the platform equivalent). Run the
desktop app once to set up providers, or use `--ollama` /
`--openai-compat-url` and skip that step.

Flag reference: [cli-flags.md](../dev/reference/cli-flags.md).

## Related

- [Sub-agents](./sub-agents.md)
- [Getting started](./getting-started.md)
