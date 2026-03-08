# chatty-tui

A lightweight terminal chat interface for Chatty. Single-session, no persistence — launch it, chat with an LLM, exit. Think of it as a terminal-native companion to the `chatty-gpui` desktop app.

## Installation

### Pre-built binaries

Download the latest release for your platform from the [GitHub Releases page](https://github.com/boersmamarcel/chatty2/releases):

- **macOS (Apple Silicon):** `.dmg`
- **Linux (x86_64):** `.AppImage`
- **Windows (x86_64):** `.exe` installer

### From source

Requires [Rust](https://rustup.rs/) (stable toolchain).

```bash
git clone https://github.com/boersmamarcel/chatty2
cd chatty2
cargo install --path crates/chatty-tui
```

This installs `chatty-tui` to `~/.cargo/bin/`, which should already be on your `PATH` if you installed Rust via rustup.

## Building

```bash
# Debug build
cargo build -p chatty-tui

# Release build
cargo build -p chatty-tui --release
```

The binary is output as `target/{debug,release}/chatty-tui`.

## Running

### Interactive mode

```bash
# Uses the first configured model
chatty-tui

# Specify a model by name, ID, or partial identifier
chatty-tui --model claude-3.5-sonnet
chatty-tui --model "Claude 3.5 Sonnet"
```

### Headless mode

Send a single message and print the response to stdout:

```bash
chatty-tui --headless -m "Explain Rust ownership in one paragraph"
```

### Pipe mode

Read from stdin, send as a message, print the response:

```bash
echo "Summarize this code" | chatty-tui --pipe
cat src/main.rs | chatty-tui --pipe
```

## Prerequisites

chatty-tui shares configuration with the desktop app. You need:

1. **At least one provider configured** — API keys and provider settings are read from the same JSON config files as chatty-gpui (stored in `~/.config/chatty/` or platform equivalent).
2. **At least one model configured** — run the desktop app once to set up providers and models, or edit the config files directly.

MCP servers configured in the desktop app are also available in chatty-tui.

## Keybindings

| Key | Action |
|:----|:-------|
| `Enter` | Send message |
| `Ctrl+C` | Stop streaming response / quit if idle |
| `Ctrl+Q` | Quit immediately |
| `y` / `n` | Approve / deny tool execution (during approval prompt) |

## Architecture

```
main.rs        CLI args (clap), Tokio runtime, settings loading
app.rs         Ratatui render loop + crossterm input + event mux
engine.rs      ChatEngine — single-conversation logic, stream processing
events.rs      AppEvent enum (channel-based, replaces GPUI EventEmitter)
headless.rs    Headless/pipe mode for non-interactive usage
ui/            Ratatui widgets (chat view, input, status bar, approval)
```

`ChatEngine` is UI-agnostic — it powers both the interactive TUI and headless mode, and is designed for future sub-agent reuse.

### Event flow

```
User input ──► ChatEngine.send_message()
                    │
                    ├── Spawns tokio task: stream_prompt() loop
                    │   └── StreamChunk ──► AppEvent (via mpsc channel)
                    │
                    ▼
               Main loop (tokio::select!)
                    │
                    ├── crossterm events ──► key handling
                    ├── AppEvent ──► ChatEngine.handle_event() ──► update state
                    └── tick ──► redraw
```
