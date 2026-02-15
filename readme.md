# Chatty

A desktop chat application built with Rust and GPUI, featuring multi-provider LLM support and extensible tool integration.

## Features

- **Multi-Provider Support**: Connect to OpenAI, Anthropic, Google Gemini, Mistral, and Ollama
- **Local and Cloud Models**: Use both cloud-based and locally-hosted LLMs
- **Extensible Tool System**: Built-in tools plus MCP (Model Context Protocol) integration
- **GPU-Accelerated UI**: Built on GPUI for smooth, responsive interface
- **Conversation Management**: Persistent conversations with cost tracking
- **File Attachments**: Support for images and PDFs (provider-dependent)

## Supported Tools

Chatty provides LLMs with access to various tools, organized into three categories:

### 1. Built-in Filesystem Tools

Read-only tools for file exploration:
- **read_file**: Read text file contents
- **read_binary**: Read binary files as base64
- **list_directory**: List directory contents with metadata
- **glob_search**: Search for files using glob patterns (e.g., `**/*.rs`)

Write tools (require approval):
- **write_file**: Create or overwrite files
- **apply_diff**: Apply unified diff patches
- **create_directory**: Create new directories
- **delete_file**: Delete files or directories
- **move_file**: Move or rename files

### 2. Bash Execution Tool

- **bash**: Execute shell commands in a configurable workspace directory
  - Requires workspace directory configuration
  - Supports approval modes: auto-approve, prompt, or deny
  - Sandboxed execution option available
  - Real-time streaming output

### 3. MCP (Model Context Protocol) Tools

Chatty integrates with MCP servers to provide dynamic tool capabilities:

- **Automatic Discovery**: MCP servers are configured in settings and auto-discovered at startup
- **Dynamic Tools**: Each MCP server can expose multiple tools (e.g., GitHub operations, database queries)
- **Tool Metadata**: Servers provide their own tool descriptions and schemas
- **Live Connections**: Tools remain active throughout conversation lifecycle

**Popular MCP Servers**:
- `@modelcontextprotocol/server-github`: GitHub operations (issues, PRs, code search)
- `@modelcontextprotocol/server-filesystem`: Advanced filesystem operations
- `@modelcontextprotocol/server-postgres`: PostgreSQL database queries
- `@modelcontextprotocol/server-brave-search`: Web search capabilities
- Custom servers: Write your own MCP servers in any language

### 4. Meta Tool

- **list_tools**: Lists all available tools and their schemas (useful for LLM self-discovery)

## Configuration

### Enabling Filesystem Tools

1. Open Settings → Execution
2. Set workspace directory (absolute path required)
3. Enable code execution
4. Configure approval mode:
   - **Auto-approve**: Tools execute immediately
   - **Prompt**: Show approval dialog for each operation
   - **Deny**: Block all tool execution

### Adding MCP Servers

1. Open Settings → MCP Servers
2. Click "Add Server"
3. Configure:
   - **Name**: Identifier for the server
   - **Command**: Executable path (e.g., `npx`)
   - **Args**: Command arguments (e.g., `-y @modelcontextprotocol/server-github`)
   - **Environment**: Optional environment variables

Example MCP server configurations:

**GitHub Server**:
```json
{
  "name": "github",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-github"],
  "env": {
    "GITHUB_TOKEN": "ghp_your_token_here"
  }
}
```

**Brave Search Server**:
```json
{
  "name": "brave-search",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-brave-search"],
  "env": {
    "BRAVE_API_KEY": "your_brave_api_key"
  }
}
```

**PostgreSQL Server**:
```json
{
  "name": "postgres",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-postgres"],
  "env": {
    "POSTGRES_CONNECTION_STRING": "postgresql://user:pass@localhost/dbname"
  }
}
```

**Filesystem Server** (alternative to built-in tools):
```json
{
  "name": "filesystem",
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/allowed/directory"],
  "env": {}
}
```

**Custom Local Server**:
```json
{
  "name": "my-custom-server",
  "command": "/usr/local/bin/python",
  "args": ["/path/to/my_mcp_server.py"],
  "env": {
    "API_KEY": "custom_key",
    "DEBUG": "true"
  }
}
```

## Supported Providers

### OpenAI
- Models: GPT-4, GPT-4 Turbo, GPT-3.5, o1, o3-mini
- Capabilities: Images (PNG, JPEG, GIF, WebP), Temperature control
- Note: PDF support is "lossy" (converts to images)

### Anthropic
- Models: Claude 3.5 Sonnet, Claude 3 Opus, Claude 3 Haiku
- Capabilities: Images, PDFs (native), Temperature control
- Best for: Long-context tasks, code generation

### Google Gemini
- Models: Gemini 1.5 Pro, Gemini 1.5 Flash
- Capabilities: Images, PDFs, Temperature control
- Best for: Multimodal tasks, long context

### Mistral
- Models: Mistral Large, Mistral Medium, Mistral Small
- Capabilities: Text-only, Temperature control
- Best for: European data residency

### Ollama
- Models: Any locally-installed Ollama model
- Capabilities: Varies by model (vision models support images)
- Best for: Privacy, offline usage, experimentation
- Note: Capabilities auto-detected per model

## Development

Built with:
- **UI Framework**: [GPUI](https://crates.io/crates/gpui) - Zed's GPU-accelerated UI framework
- **LLM Integration**: [rig-core](https://crates.io/crates/rig-core) for multi-provider support
- **MCP Integration**: [rmcp](https://crates.io/crates/rmcp) for Model Context Protocol
- **Async Runtime**: Tokio
- **Serialization**: serde/serde_json

### Build Commands

```bash
# Development build
cargo build

# Release build
cargo build --release

# Run tests
cargo test

# Code quality
cargo fmt --check
cargo clippy -- -D warnings
```

### Packaging

```bash
# macOS (.app bundle and .dmg)
./scripts/package-macos.sh

# Linux (.tar.gz)
./scripts/package-linux.sh
```

## Architecture

- **Event-Driven**: Uses GPUI's reactive system and Tokio async runtime
- **Global State**: Leverages GPUI globals for app-wide state (providers, models, settings)
- **Streaming**: Real-time LLM response streaming with tool call interleaving
- **Persistence**: JSON-based storage for conversations, settings, and configuration
- **Math Rendering**: LaTeX expressions rendered to SVG using Typst with platform-specific caching

See [CLAUDE.md](CLAUDE.md) for detailed architecture documentation and coding patterns.

## License

MIT
