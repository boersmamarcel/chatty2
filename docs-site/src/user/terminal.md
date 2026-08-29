# Terminal interface (`chatty-tui`)

**When to read this:** Use Chatty from the terminal — interactive, headless, or piped.

`chatty-tui` shares provider and model configuration with the desktop app.

## Modes

| Mode | Command | Description |
|------|---------|-------------|
| **Interactive** | `chatty-tui` | Full-screen TUI with scrollable chat, model picker, approvals |
| **Headless** | `chatty-tui --headless -m "question"` | Single message → stdout (scripts, sub-agents) |
| **Pipe** | `cat file.rs \| chatty-tui --pipe` | Read stdin as message, print response |

## Zero-config quick start

Connect directly to a running model server — no desktop setup:

```bash
# Ollama (auto-discovers localhost:11434)
chatty-tui --ollama
chatty-tui --ollama --model llama3.2

# vLLM / llama.cpp / LM Studio (OpenAI-compatible)
chatty-tui --openai-compat-url http://localhost:8000
chatty-tui --openai-compat-url http://localhost:8000 --model my-model --api-key sk-...
```

## Installing

| Method | How |
|--------|-----|
| **Desktop app** | macOS: Chatty menu → Install CLI. Linux/Windows: Settings → General → Install CLI |
| **Releases** | Same package as desktop app includes `chatty-tui` |
| **Source** | `cargo install --path crates/chatty-tui` |

Installed to `~/.local/bin` on Linux or your user bin directory on Windows.

## Welcome screen & status bar

Interactive mode with an empty conversation shows: active model, context window, workspace, git branch, enabled tools, internet capabilities, runtime features (memory, modules, agents). MCP and memory load in the background — badges show `⟳` while initializing.

Status bar: app version, cwd (truncated), git branch. Footer hints update during streaming (`Ctrl+C stop`).

## Keybindings (interactive)

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `/` | Slash-command picker |
| `@` | File picker (filter with typing) |
| `PageUp`/`PageDown`, `Shift+↑/↓`, mouse wheel | Scroll |
| `End` | Jump to bottom, resume auto-scroll |
| `y` / `n` | Approve / deny tool prompt |
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

`chatty-tui` reads the same config as the desktop app (`~/.config/chatty/` or platform equivalent). Run the desktop app once to set up providers, or use `--ollama` / `--openai-compat-url` to skip configuration.

CLI reference: [cli-flags.md](../dev/reference/cli-flags.md).

## Related

- [Sub-agents](./sub-agents.md)
- [Getting started](./getting-started.md)
