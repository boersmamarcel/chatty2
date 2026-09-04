# Terminal interface (`chatty-tui`)

**When to read this:** Use Chatty from a terminal — interactive, headless, or
piped. Shares provider and model config with the desktop app.

## Modes

| Mode | Command | Description |
|------|---------|-------------|
| **Interactive** | `chatty-tui` | Full-screen TUI: scrollable chat, model picker, tool picker, approvals |
| **Headless** | `chatty-tui --headless -m "question"` | One message → stdout (scripts, [sub-agents](./sub-agents.md)) |
| **Pipe** | `cat file.rs \| chatty-tui --pipe` | stdin as the message, print the response |

## Zero-config quick start

Connect to a running model server without opening the desktop app:

```bash
# Ollama (discovers models at localhost:11434)
chatty-tui --ollama
chatty-tui --ollama --model llama3.2

# vLLM / llama.cpp / LM Studio (OpenAI-compatible)
chatty-tui --openai-compat-url http://localhost:8000
chatty-tui --openai-compat-url http://localhost:8000 --model my-model --api-key sk-...
```

## Installing

| Method | How |
|--------|-----|
| **Desktop app** | macOS: **Chatty** menu → **Install CLI**. Linux/Windows: **Settings → General → Install CLI…** |
| **Releases** | Same package as the desktop app includes `chatty-tui` |
| **Source** | `cargo install --path crates/chatty-tui` |

Linux install path is `~/.local/bin`; Windows uses the user bin directory.

## Welcome screen & status bar

Interactive mode with an empty conversation shows: active model and context
window, workspace, git branch, enabled tools (shell, fs-read/write, git, code,
docker), internet capabilities (fetch, search, browser-use, daytona, MCP), and
runtime features (memory, modules, remote agents).

MCP, memory, and embeddings load in the background. While they initialize,
badges show `⟳` (e.g. `[MCP ⟳]`) and the status bar reads `● loading
services…`. The status bar always has app version, cwd (truncated), and git
branch when inside a repo. Footer hints switch to `Ctrl+C stop` while
streaming. A scrollbar appears when the transcript overflows; scrolling up
unpins auto-scroll, `End` (or scrolling back down) re-pins it.

## Keybindings (interactive)

| Key | Action |
|-----|--------|
| `Enter` | Send message |
| `/` | Slash-command picker (`↑/↓`, `Tab` or `Enter`) |
| `@` | File picker (type to filter) |
| `PageUp` / `PageDown`, `Shift+↑/↓`, mouse wheel | Scroll |
| `End` | Jump to bottom, resume auto-scroll |
| `y` / `n` | Approve / deny a tool prompt |
| `1`-`9` | Pick an option when the agent asks a clarifying question (`ask_user`) |
| `t` | Type a custom answer instead of picking an option |
| `Ctrl+C` | Stop streaming (or quit if idle) |
| `Ctrl+Q` | Quit immediately |

A pending clarifying question replaces the input row and takes over the
keyboard until answered — `Ctrl+C`/`Ctrl+Q` still work. Multiple questions are
answered one at a time; press `Esc` while typing a custom answer to go back
to the options.

Launch overrides: `--enable tool1,tool2` / `--disable tool1,tool2`.

## Slash commands (TUI)

| Command | Action |
|---------|--------|
| `/model [query]` | Switch model (`/model` opens the picker) |
| `/tools [name]` | Toggle tool groups |
| `/modules [show\|enable\|disable\|dir <path>\|port <n>]` | Module runtime |
| `/add-dir <directory>` | Expand workspace |
| `/agent <prompt>` | Headless sub-agent |
| `/clear`, `/new` | Fresh conversation |
| `/compact` | Summarize older messages |
| `/context` | Token usage and cwd |
| `/copy` | Copy latest response |
| `/update` | CLI auto-update (Linux: refresh `~/.local/bin/chatty-tui`) |
| `/cwd`, `/cd [directory]` | Show or change cwd |
| `/quit`, `/exit` | Quit (works while streaming) |

## Config sharing

`chatty-tui` reads the same config files as the desktop app
(`~/.config/chatty/` or the platform equivalent). Run the desktop app once to
set providers, or skip that with `--ollama` / `--openai-compat-url`.

CLI reference: [cli-flags.md](../dev/reference/cli-flags.md).

## Related

- [Sub-agents](./sub-agents.md)
- [Getting started](./getting-started.md)
