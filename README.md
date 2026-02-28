<p align="center">
  <img src="assets/app_icon/ai-2.png" alt="Chatty" width="128" height="128">
</p>

<h1 align="center">Chatty</h1>
 
<p align="center">
  <strong>A fast, native desktop chat client for LLMs — built with Rust and GPU-accelerated rendering.</strong>
</p>

<p align="center">
  <a href="#getting-started">Getting Started</a> &bull;
  <a href="#why-chatty">Why Chatty</a> &bull;
  <a href="#features">Features</a> &bull;
  <a href="#tools--mcp">Tools & MCP</a> &bull;
  <a href="#development">Development</a>
</p>

---

<!-- TODO: Add a hero screenshot or GIF here showing the main chat interface -->
<!-- <p align="center"><img src="assets/screenshots/hero.png" alt="Chatty screenshot" width="800"></p> -->

## Getting Started

### 1. Download

Grab the latest release from [GitHub Releases](https://github.com/boersmamarcel/chatty2/releases):

| Platform | Format |
|:---------|:-------|
| macOS (Intel & Apple Silicon) | `.dmg` installer |
| Linux (x86_64) | `.tar.gz` archive |
| Windows (x86_64) | `.exe` installer |

### 2. Add a Provider

When you first launch Chatty, you'll need to connect at least one LLM provider.

1. Click the **gear icon** in the title bar to open Settings
2. Go to the **Providers** tab
3. Click **Add Provider** and select one (e.g., OpenAI, Anthropic, Ollama)
4. Paste your API key (not needed for Ollama — it connects to your local instance automatically)

<!-- TODO: Add GIF or screenshot showing the provider setup flow -->
<!-- <p align="center"><img src="assets/screenshots/add-provider.gif" alt="Adding a provider" width="600"></p> -->

### 3. Add a Model

After adding a provider, you need to tell Chatty which model(s) to use.

1. Still in Settings, go to the **Models** tab
2. Click **Add Model**
3. Pick a provider and enter a model ID (e.g., `gpt-4o`, `claude-sonnet-4-20250514`, `gemini-2.0-flash`)
4. Chatty auto-detects capabilities like vision and PDF support — no extra config needed

<!-- TODO: Add GIF or screenshot showing model setup -->
<!-- <p align="center"><img src="assets/screenshots/add-model.gif" alt="Adding a model" width="600"></p> -->

### 4. Start Chatting

Close Settings and type your first message. You can switch between models using the model selector at the bottom of the chat.

<!-- TODO: Add GIF showing a first conversation with model switching -->
<!-- <p align="center"><img src="assets/screenshots/first-chat.gif" alt="First conversation" width="600"></p> -->

### 5. Enable Tools (Optional)

Chatty can give your LLM access to the filesystem, a sandboxed shell, and MCP servers. This is off by default.

1. Go to **Settings > Execution**
2. Set a **workspace directory** — the LLM can only access files inside this folder
3. Toggle **code execution** on
4. Choose an approval mode:
   - **Ask every time** — you approve each tool call (recommended to start)
   - **Auto-approve** — tools run without prompting
   - **Deny all** — tools are visible but blocked

See [Tools & MCP](#tools--mcp) below for details.

---

## Why Chatty?

**Your keys, your data.** No middleman, no subscriptions. Talk directly to OpenAI, Anthropic, Google, Mistral, or your local Ollama instance. See exactly what each conversation costs with per-message token tracking and running cost totals in the sidebar.

**Native Rust performance.** Not another Electron wrapper — built on [GPUI](https://crates.io/crates/gpui), the GPU-accelerated framework behind the Zed editor. Instant startup, smooth scrolling, minimal memory footprint.

**One app, every model.** Switch between Claude, GPT-4, Gemini, Mistral, and Ollama models mid-conversation. Compare answers, use the right model for the job — all from a single window.

**Real tool use, properly sandboxed.** Give your LLM filesystem access, a bash shell, and MCP servers — all within a workspace sandbox. On Linux, shell commands run inside [bubblewrap](https://github.com/containers/bubblewrap) with namespace isolation. On macOS, they use `sandbox-exec` with policy profiles that block access to `.ssh`, `.aws`, and other sensitive directories. You choose the approval mode: ask every time, auto-approve, or deny all.

**Multi-turn agents.** Your LLM can chain up to 10 tool calls per response — read files, run commands, analyze results, and iterate. It can even fetch web pages and attach images or PDFs it finds for its own analysis.

**Privacy first.** Run fully local with Ollama — no data leaves your machine. No telemetry, no tracking. Conversations are stored as local JSON files, never uploaded anywhere.

---

## Features

### Multi-Provider Support

Connect to multiple LLM providers from a single interface. Chatty auto-detects per-model capabilities (vision, PDF support, temperature) so the UI always shows the right options.

| Provider | Image Support | PDF Support | Temperature | Notes |
|:---------|:---:|:---:|:---:|:------|
| **OpenAI** | Yes | Lossy | Yes | GPT-4, GPT-4 Turbo, o1, o3-mini |
| **Anthropic** | Yes | Native | Yes | Claude 3.5 Sonnet, Claude 3 Opus/Haiku |
| **Google Gemini** | Yes | Native | Yes | Gemini 1.5 Pro, Gemini 1.5 Flash |
| **Mistral** | — | — | Yes | Mistral Large, Medium, Small |
| **Azure OpenAI** | Yes | Lossy | Yes | API Key or Entra ID auth |
| **Ollama** | Per-model | Per-model | — | Auto-detected capabilities, fully local |

### Rich Rendering

- **Markdown** with full formatting
- **Syntax-highlighted code blocks** (100+ languages) with one-click copy
- **LaTeX math** — inline (`$...$`) and block (`$$...$$`) rendered to crisp SVGs via Typst
- **Image and PDF** previews inline in chat

### Conversations & Cost Tracking

- Persistent conversations saved locally as JSON — nothing is stored remotely
- Auto-generated conversation titles
- **Per-conversation cost tracking** displayed in the sidebar — see running totals at a glance
- **Per-message token usage** — input and output token counts with cost breakdown
- Cost calculations use your model's actual pricing (cost per million input/output tokens)
- **Regeneration tracking** — original assistant responses are captured automatically when regenerated, creating DPO preference pairs for model fine-tuning

### Training Data Export (ATIF)

Chatty can export conversations in [ATIF (Agent Trajectory Interchange Format)](https://harborframework.com/docs/agents/trajectory-format), a structured JSON format designed for agent training data pipelines. Each export captures:

- **Messages** — user and agent steps with full content
- **Tool calls** — function name, arguments, and output for every tool invocation
- **Reasoning** — chain-of-thought thinking blocks from extended thinking
- **Timestamps** — per-message Unix timestamps
- **Token metrics** — per-step and aggregate input/output token counts with cost
- **Feedback** — thumbs up/down signals per assistant message
- **Regeneration pairs** — original (rejected) vs. replacement (chosen) responses for DPO fine-tuning

ATIF trajectories feed into external training pipelines, Harbor Framework workflows, and the planned in-app fine-tuning system.

### Training Data Export (JSONL)

Chatty can also export conversations in JSONL format for direct use with fine-tuning APIs:

- **SFT (Supervised Fine-Tuning)** — conversations in ChatML format (`{"messages": [{"role": "...", "content": "..."}]}`) compatible with OpenAI, Anthropic, Together AI, and other fine-tuning services
- **DPO (Direct Preference Optimization)** — preference pairs from regenerated responses (`{"prompt": [...], "chosen": "...", "rejected": "..."}`) for RLHF training

Key features:
- **Automatic deduplication** — re-exported conversations replace previous entries (keyed by `_conversation_id`)
- **Multimodal stripping** — images and PDFs are stripped, keeping only text content (most fine-tuning APIs don't support multimodal inputs)
- **Tool call support** — optionally include tool calls and results in ChatML format

SFT data is appended to `sft.jsonl` and DPO pairs to `dpo.jsonl` in the exports directory:

- **macOS**: `~/Library/Application Support/chatty/exports/`
- **Linux**: `~/.config/chatty/exports/` (or `$XDG_CONFIG_HOME/chatty/exports/`)
- **Windows**: `%APPDATA%\chatty\exports\`

Enable auto-export in **Settings > Training Data**.

### Thinking & Traces

- **Extended thinking** blocks for Claude's chain-of-thought reasoning
- **Tool call traces** showing input, output, duration, and status
- Collapsible so they don't clutter the conversation

### Themes

20+ themes with light and dark variants: Ayu, Catppuccin, Everforest, Flexoki, Gruvbox, Matrix, Solarized, TokyoNight, and more. Configurable font size.

### Auto-Updates

Background update checks against GitHub releases with one-click install. Downloads are verified with SHA-256 checksums before installation. On macOS, the update replaces the app bundle and relaunches automatically.

---

## Tools & MCP

### Built-in Tools

When code execution is enabled in Settings, your LLM can use these tools (all scoped to your configured workspace directory):

| Tool | What it does | Requires approval |
|:-----|:-------------|:-:|
| `read_file` | Read text file contents | No |
| `read_binary` | Read binary files as base64 | No |
| `list_directory` | List directory contents with metadata | No |
| `glob_search` | Search files using glob patterns (e.g., `**/*.rs`) | No |
| `write_file` | Create or overwrite files | Yes |
| `apply_diff` | Apply unified diff patches | Yes |
| `create_directory` | Create directories | Yes |
| `delete_file` | Delete files or directories | Yes |
| `move_file` | Move or rename files | Yes |
| `bash` | Execute shell commands (sandboxed, streaming output) | Yes |
| `list_tools` | Lists all available tools and schemas | No |

### MCP Servers

[MCP (Model Context Protocol)](https://modelcontextprotocol.io/) lets your LLM interact with external tools and data sources. Chatty has first-class MCP support — configure a server in Settings and the tools become available to your model automatically.

**To add an MCP server:**

1. Go to **Settings > MCP Servers**
2. Click **Add Server**
3. Enter the server command, args, and any environment variables
4. Enable the server when you're ready to use it

MCP management tools (enable in Settings > Execution) let the LLM itself add, edit, list, and delete MCP servers — with env var masking so your API keys are never exposed.

<details>
<summary><strong>Recommended MCP Servers</strong></summary>

Here are MCP servers that pair well with Chatty:

#### GitHub (`@modelcontextprotocol/server-github`)
Search code, browse issues and PRs, read file contents from repos.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "ghp_..." } }
```

#### Filesystem (`@modelcontextprotocol/server-filesystem`)
Advanced filesystem operations beyond Chatty's built-in tools.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"] }
```

#### PostgreSQL (`@modelcontextprotocol/server-postgres`)
Run read-only SQL queries against your PostgreSQL databases.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-postgres"], "env": { "POSTGRES_CONNECTION_STRING": "postgresql://user:pass@localhost/db" } }
```

#### Brave Search (`@modelcontextprotocol/server-brave-search`)
Give your LLM access to web search for current events and documentation.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-brave-search"], "env": { "BRAVE_API_KEY": "..." } }
```

#### Memory (`@modelcontextprotocol/server-memory`)
Persistent key-value memory across conversations.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-memory"] }
```

#### Puppeteer (`@modelcontextprotocol/server-puppeteer`)
Browser automation — navigate pages, take screenshots, extract content.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-puppeteer"] }
```

#### Slack (`@modelcontextprotocol/server-slack`)
Read and search Slack messages, channels, and threads.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-slack"], "env": { "SLACK_BOT_TOKEN": "xoxb-..." } }
```

#### Google Maps (`@modelcontextprotocol/server-google-maps`)
Geocoding, directions, place search, and distance calculations.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-google-maps"], "env": { "GOOGLE_MAPS_API_KEY": "..." } }
```

#### SQLite (`@modelcontextprotocol/server-sqlite`)
Query and explore SQLite databases.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-sqlite"], "env": { "SQLITE_DB_PATH": "/path/to/database.db" } }
```

#### Fetch (`@modelcontextprotocol/server-fetch`)
Fetch and convert web pages to markdown for the LLM to read.
```json
{ "command": "npx", "args": ["-y", "@modelcontextprotocol/server-fetch"] }
```

> **Tip:** You can also write your own MCP servers in any language. See the [MCP specification](https://modelcontextprotocol.io/) for details.

</details>

### Security

- **API key masking** — MCP env vars containing keys, tokens, or secrets are shown to the LLM as `****`
- **Workspace sandboxing** — filesystem and bash tools can only access the directory you configure
- **Shell sandboxing** — on Linux, commands run inside [bubblewrap](https://github.com/containers/bubblewrap) with full namespace isolation (process, network, mount). On macOS, `sandbox-exec` blocks access to `.ssh`, `.aws`, `.gnupg`, and other sensitive paths
- **Optional network isolation** — block shell commands from making network requests entirely
- **MCP servers disabled by default** — servers added by the LLM must be manually enabled
- **No telemetry** — nothing is sent anywhere except directly to your configured LLM provider

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
./scripts/package-macos.sh        # macOS (.app + .dmg)
./scripts/package-linux.sh        # Linux (.tar.gz)
./scripts/package-windows.ps1     # Windows (.exe installer, run in PowerShell)
```

### CI/CD & Releasing

PRs to `main` run CI automatically (tests, formatting, clippy, AI code review). Cargo dependencies are cached across runs.

**To release a new version**, add a label to your PR before merging:

| Label | Effect |
|:------|:-------|
| `release:patch` | Bump `0.1.52` → `0.1.53` |
| `release:minor` | Bump `0.1.52` → `0.2.0` |
| `release:major` | Bump `0.1.52` → `1.0.0` |

Merging the PR triggers the full pipeline: version bump → changelog generation → git tag → GitHub Release → cross-platform builds (Linux AppImage, macOS DMG, Windows EXE).

You can also trigger a release manually from **Actions → Prepare Release → Run workflow**.

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
<!-- release-flow-test -->
