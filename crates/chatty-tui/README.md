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
# Uses the model marked default in the desktop app (else the first configured)
chatty-tui

# Specify a model by name, ID, or partial identifier
chatty-tui --model claude-3.5-sonnet
chatty-tui --model "Claude 3.5 Sonnet"
```

### Zero-config with Ollama

Connect directly to a running Ollama instance — no desktop app setup needed:

```bash
# Auto-discover models from local Ollama (localhost:11434)
chatty-tui --ollama

# Pick a specific Ollama model
chatty-tui --ollama --model llama3.2

# Connect to a remote Ollama instance
chatty-tui --ollama http://remote-host:11434
```

### Zero-config with vllm / llama.cpp / LM Studio

Connect to any OpenAI-compatible server:

```bash
# vllm
chatty-tui --openai-compat-url http://localhost:8000

# llama.cpp
chatty-tui --openai-compat-url http://localhost:8080

# Pick a specific model
chatty-tui --openai-compat-url http://localhost:8000 --model my-model

# With API key (if required)
chatty-tui --openai-compat-url https://api.example.com --api-key sk-...
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

### Delegation: `--broker` and `--team`

A leader delegates through its `invoke_agent` tool to `local-agent` or to a named
virtual agent; each delegation is a fresh `chatty-tui` worker process. The desktop
app's module settings start that broker; a terminal leader starts its own with
`--broker` (Unix only, valid with `--headless`, `--pipe` and the interactive TUI):

```bash
chatty-tui --headless --broker -m "Refactor the auth module and write tests"
```

`--team <id>` runs a fixed roster from a team directory (`teams/<id>/team.json` +
`SKILL.md`) and implies `--broker`. Searched in `<workspace>/.chatty/teams/`, then
the platform data directory's `chatty/teams/`, then the compiled-in presets
(`coder-reviewer`):

```bash
chatty-tui --team coder-reviewer --headless --ollama --model qwen3:14b \
  -m "Fix the overdraft bug in src/account.py; tests/test_account.py must pass."
```

Flags a worker is started with (a leader rarely passes these by hand):
`--workspace <DIR>` (its own `git worktree`), `--participant-socket <PATH>` +
`--participant-name <NAME>` (register with the broker and wait for one task),
`--tools <profile>` (`coordinator` / `coder` / `reviewer` allowlist), `--preamble
<text>` (role instructions) and `--max-agent-turns <n>` (that worker's own turn
budget, AGE-440). `--tools` / `--preamble` / `--model` on a `--team` leader override
the team file's leader settings. Design and file format:
[`docs/a2a-and-wasm-modules.md`](../../docs/a2a-and-wasm-modules.md).

## Prerequisites

chatty-tui shares configuration with the desktop app. You need:

1. **At least one provider configured** — API keys and provider settings are read from the same JSON config files as chatty-gpui (stored in `~/.config/chatty/` or platform equivalent).
2. **At least one model configured** — run the desktop app once to set up providers and models, or edit the config files directly.

**Or use `--ollama` / `--openai-compat-url`** to skip all configuration and connect directly to a running model server.

MCP servers configured in the desktop app are also available in chatty-tui.

## Keybindings

| Key | Action |
|:----|:-------|
| `Enter` | Send message |
| `/` | Slash-command picker; `@` opens the file picker |
| `PageUp` / `PageDown`, `Shift+↑/↓`, mouse wheel | Scroll the transcript |
| `End` | Jump to the bottom and resume auto-scroll |
| `Ctrl+C` | Stop streaming response / quit if idle |
| `Ctrl+Q` | Quit immediately |
| `Ctrl+R` | Toggle folded tool calls vs full payloads (same as `/verbose`) |
| `y` / `n` | Approve / deny tool execution (during approval prompt) |
| `1`-`9`, `t`, `Esc` | Pick an option, type a custom answer, or go back when the agent asks a clarifying question (`ask_user`) |

## Slash commands

Typing `/` in the input opens an inline slash-command menu. Use `↑/↓` to select and `Tab` or `Enter` to apply.

| Command | Action |
|:--------|:-------|
| `/model [query]` | Switch model (`/model` opens picker) |
| `/tools [name]` | Toggle tool groups (`/tools` opens picker) |
| `/add-dir <directory>` | Expand workspace access to include a directory |
| `/modules …` | Module runtime settings (enable, directory, gateway port) |
| `/agent [name] <prompt>` | Launch a headless `chatty-tui` sub-agent, or send the prompt to a named A2A agent |
| `/clear`, `/new` | Clear conversation history and start fresh |
| `/compact` | Summarize older messages to reduce context usage |
| `/context` | Show token/context usage and current working directory |
| `/copy` | Copy the latest assistant response to system clipboard |
| `/update` | Trigger CLI auto-update when an installed CLI target exists |
| `/cwd`, `/cd [directory]` | Show or change the working directory |
| `/online [url\|off]` | Show where this conversation runs; move it to a `chatty-server` or back. Developer-only until `hosted_conversations_enabled` is on (AGE-298/308) |
| `/verbose` | Toggle folded tool-call summaries vs full payloads |
| `/paste [n]` | Print the full text of an elided long paste |
| `/quit`, `/exit` | Quit |

## Architecture

```
main.rs        CLI args (clap), Tokio runtime, settings loading, --broker/--team wiring
app.rs         Ratatui render loop + crossterm input + event mux
engine/        ChatEngine — single-conversation logic, stream processing,
               slash commands (commands.rs), typed transcript blocks (transcript.rs)
events.rs      AppEvent enum (channel-based, replaces GPUI EventEmitter)
headless/      Headless/pipe/worker turn loop, tool-result formatting,
               answer-file early stop, recovery
participant/   The broker half: Broker::start (gateway + participant socket),
               team presets, the input-required chain, delegation equivalence tests
ui/            Ratatui widgets (chat view, input, status bar, approval, plan card,
               clarification, pickers)
```

`ChatEngine` is UI-agnostic — it powers the interactive TUI, headless mode and every
delegated worker (`--participant-socket`), which is what lets a leader and its
workers be the same binary.

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
