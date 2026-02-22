<p align="center">
  <img src="assets/app_icon/ai-2.png" alt="Chatty" width="128" height="128">
</p>

<h1 align="center">Chatty</h1>

<p align="center">
  <strong>A fast, native desktop chat client for LLMs — built with Rust and GPU-accelerated rendering.</strong>
</p>

<p align="center">
  <a href="#features">Features</a> &bull;
  <a href="#why-chatty">Why Chatty</a> &bull;
  <a href="#supported-providers">Providers</a> &bull;
  <a href="#top-10-recommended-mcp-servers">MCP Servers</a> &bull;
  <a href="#getting-started">Getting Started</a> &bull;
  <a href="#development">Development</a>
</p>

---

## Why Chatty?

There are plenty of chat UIs for LLMs. Here's why Chatty stands apart:

🔑 **Your API keys, your data.** No middleman, no subscriptions. Talk directly to OpenAI, Anthropic, Google, Mistral, or your local Ollama instance.

⚡ **Native Rust performance.** Not another Electron wrapper — built on [GPUI](https://crates.io/crates/gpui), the GPU-accelerated framework behind the Zed editor. Instant startup, smooth scrolling, minimal memory.

🔀 **One app, every model.** Switch between Claude, GPT-4, Gemini, Mistral, and Ollama in a single sidebar. No more juggling tabs.

🛠️ **Built-in tools & MCP.** Filesystem access, sandboxed bash, and full [Model Context Protocol](https://modelcontextprotocol.io/) support — your LLM can read files, query databases, and more. All with an approval workflow you control.

🧠 **Transparent reasoning.** Collapsible thinking blocks and tool call traces show *exactly* what your LLM did, returned, and how long it took.

🔒 **Privacy first.** Run fully local with Ollama — no data leaves your machine. Network isolation and workspace sandboxing keep you in control.

---

## Features

### Multi-Provider LLM Support
Connect to **OpenAI**, **Anthropic**, **Google Gemini**, **Mistral**, **Azure OpenAI**, and **Ollama** — all from a single interface. Chatty auto-detects per-model capabilities (vision, PDF support, temperature) so the UI always shows the right options.

### Rich Rendering
- **Markdown** with full formatting support
- **Syntax-highlighted code blocks** (100+ languages) with one-click copy
- **LaTeX math** — inline (`$...$`) and block (`$$...$$`) expressions rendered to crisp SVGs via Typst
- **Image and PDF** previews inline in chat

### Conversation Management
- Persistent conversations saved locally as JSON
- Auto-generated titles
- Per-conversation **cost tracking** displayed in the sidebar
- Token usage breakdowns (input/output) per message

### Tool Execution
- **Filesystem tools**: read, write, search, diff — all within a configurable workspace
- **Bash execution**: sandboxed shell commands with streaming output
- **Approval workflow**: auto-approve, prompt-per-action, or deny-all modes
- **MCP integration**: connect any MCP server for dynamic tool capabilities

### Security
- **API key masking**: MCP server env vars containing sensitive keys (API keys, tokens, secrets, passwords) are automatically masked as `****` before being shown to the LLM — the real values never leave your machine
- **Sentinel preservation**: when the LLM edits an MCP server and returns `****` for a key, the original stored value is preserved — accidental overwrites are impossible
- **New servers disabled by default**: MCP servers added by the LLM start as disabled — you must explicitly enable them in Settings before they run
- **Workspace sandboxing**: filesystem and bash tools are scoped to a directory you choose; the LLM cannot access anything outside it
- **No telemetry**: nothing is sent anywhere except directly to the LLM provider you configured

### Thinking & Traces
- **Extended thinking** blocks for Claude's chain-of-thought reasoning
- **Tool call traces** with input, output, duration, and status
- **Approval blocks** inline in the conversation

### 20+ Themes
Ayu, Catppuccin, Everforest, Flexoki, Gruvbox, Matrix, Solarized, TokyoNight, and many more — each with light and dark variants. Configurable font size.

### Auto-Updates
Background update checks against GitHub releases with one-click install.

---

## Supported Providers

| Provider | Image Support | PDF Support | Temperature | Notes |
|:---------|:---:|:---:|:---:|:------|
| **OpenAI** | Yes | Lossy | Yes | GPT-4, GPT-4 Turbo, o1, o3-mini |
| **Anthropic** | Yes | Native | Yes | Claude 3.5 Sonnet, Claude 3 Opus/Haiku |
| **Google Gemini** | Yes | Native | Yes | Gemini 1.5 Pro, Gemini 1.5 Flash |
| **Mistral** | — | — | Yes | Mistral Large, Medium, Small |
| **Azure OpenAI** | Yes | Lossy | Yes | API Key or Entra ID auth |
| **Ollama** | Per-model | Per-model | — | Auto-detected capabilities, fully local |

---

## Top 10 Recommended MCP Servers

[MCP (Model Context Protocol)](https://modelcontextprotocol.io/) lets your LLM interact with external tools and data sources. Chatty has first-class MCP support — just configure a server in Settings and the tools become available to your model automatically.

Here are 10 MCP servers that pair well with Chatty:

### 1. GitHub (`@modelcontextprotocol/server-github`)
Search code, browse issues and PRs, read file contents from repos. Essential for developer workflows.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "ghp_..." } }
```

### 2. Filesystem (`@modelcontextprotocol/server-filesystem`)
Advanced filesystem operations beyond Chatty's built-in tools — useful when you want the MCP standard for file access.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"] }
```

### 3. PostgreSQL (`@modelcontextprotocol/server-postgres`)
Run read-only SQL queries against your PostgreSQL databases. Great for data exploration and debugging.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-postgres"], "env": { "POSTGRES_CONNECTION_STRING": "postgresql://user:pass@localhost/db" } }
```

### 4. Brave Search (`@modelcontextprotocol/server-brave-search`)
Give your LLM access to web search. Answers questions about current events, documentation lookups, and research.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-brave-search"], "env": { "BRAVE_API_KEY": "..." } }
```

### 5. Memory (`@modelcontextprotocol/server-memory`)
Persistent key-value memory across conversations. Lets the LLM remember facts, preferences, and context between sessions.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"] }
```

### 6. Puppeteer (`@modelcontextprotocol/server-puppeteer`)
Browser automation — navigate pages, take screenshots, fill forms, and extract content from the web.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-puppeteer"] }
```

### 7. Slack (`@modelcontextprotocol/server-slack`)
Read and search Slack messages, channels, and threads. Useful for catching up on discussions or finding information.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-slack"], "env": { "SLACK_BOT_TOKEN": "xoxb-..." } }
```

### 8. Google Maps (`@modelcontextprotocol/server-google-maps`)
Geocoding, directions, place search, and distance calculations. Handy for travel planning and location-based queries.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-google-maps"], "env": { "GOOGLE_MAPS_API_KEY": "..." } }
```

### 9. SQLite (`@modelcontextprotocol/server-sqlite`)
Query and explore SQLite databases. Perfect for local app databases, analytics, or prototyping data pipelines.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-sqlite"], "env": { "SQLITE_DB_PATH": "/path/to/database.db" } }
```

### 10. Fetch (`@modelcontextprotocol/server-fetch`)
Fetch and convert web pages to markdown for the LLM to read. Useful for documentation lookups and reading articles.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-fetch"] }
```

> **Tip:** You can also write your own MCP servers in any language. See the [MCP specification](https://modelcontextprotocol.io/) for details.

---

## Getting Started

### Download

Grab the latest release for your platform from [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases):

- **macOS** (Intel & Apple Silicon): `.dmg` installer
- **Linux** (x86_64): `.tar.gz` archive

### Quick Setup

1. Launch Chatty
2. Open **Settings** (gear icon in the title bar)
3. Add a provider (e.g. paste your OpenAI or Anthropic API key)
4. Add one or more models for that provider
5. Start chatting

### Enabling Tools

1. Go to **Settings > Execution**
2. Set a workspace directory (absolute path)
3. Enable code execution and pick an approval mode
4. Optionally add MCP servers under **Settings > MCP Servers**

---

## Built-in Tools

### Filesystem Tools

**Read-only:**
- `read_file` — Read text file contents
- `read_binary` — Read binary files as base64
- `list_directory` — List directory contents with metadata
- `glob_search` — Search files using glob patterns (e.g. `**/*.rs`)

**Write (require approval):**
- `write_file` — Create or overwrite files
- `apply_diff` — Apply unified diff patches
- `create_directory` — Create directories
- `delete_file` — Delete files or directories
- `move_file` — Move or rename files

### Bash Execution
- Execute shell commands in a configurable workspace
- Sandboxed execution with network isolation
- Real-time streaming output
- Configurable approval modes

### Meta Tools
- `list_tools` — Lists all available tools and their schemas (for LLM self-discovery)

### MCP Management Tools (optional, enable in Settings → Execution)
- `list_mcp_services` — Lists all configured MCP servers with masked env vars (safe for LLM)
- `add_mcp_service` — Adds a new MCP server configuration (saved as disabled; user enables it)
- `edit_mcp_service` — Edits an existing MCP server's command, args, env vars, or enabled state
- `delete_mcp_service` — Removes an MCP server configuration and stops it if running

> **Security note:** Sensitive env var values (API keys, tokens, etc.) are always masked as `****` when shown to the LLM. Sending `****` back in an edit preserves the original stored value.

---

## Development

Built with:
- **[GPUI](https://crates.io/crates/gpui)** — Zed's GPU-accelerated UI framework
- **[rig-core](https://crates.io/crates/rig-core)** — Multi-provider LLM integration
- **[rmcp](https://crates.io/crates/rmcp)** — Model Context Protocol support
- **[Typst](https://crates.io/crates/typst)** — LaTeX math rendering
- **[syntect](https://crates.io/crates/syntect)** — Syntax highlighting
- **Tokio** — Async runtime
- **serde** — Serialization/persistence

### Build Commands

```bash
cargo build            # Debug build
cargo build --release  # Release build
cargo test             # Run tests
cargo fmt --check      # Check formatting
cargo clippy -- -D warnings  # Lint
```

### Packaging

```bash
./scripts/package-macos.sh   # macOS (.app + .dmg)
./scripts/package-linux.sh   # Linux (.tar.gz)
```

### Architecture

- **Event-driven** reactive UI with GPUI's global state system
- **Centralized stream lifecycle** via `StreamManager` entity with cancellation tokens and decoupled event-driven UI updates
- **Streaming** LLM responses with interleaved tool calls
- **Optimistic updates** for instant UI feedback with async persistence
- **JSON-based** local storage for conversations, settings, and configuration
- **LaTeX to SVG** pipeline with theme-aware caching

See [CLAUDE.md](CLAUDE.md) for detailed architecture documentation and coding patterns.

---

## License

MIT
