# Agentic tools

**When to read this:** Enable filesystem, shell, MCP, and related tools, and
see what the agent can call.

The generated [tools catalog](../dev/reference/tools-catalog.md) is the
name-by-name lookup. This page is the user-facing map.

## Enable in Settings

1. **Settings → Code Execution**
2. Set a **workspace directory** (absolute path)
3. Toggle **code execution** on
4. Choose **approval mode**: ask every time / auto-approve / deny all

Most filesystem and bash tools are scoped to that workspace. Internet tools
have a separate toggle under **Settings → Search**.

![Web fetch](../assets/animations/webfetch.gif)

![Internet access settings](../assets/animations/advanced_internet_access_settings.gif)

File-edit, shell, and MCP management recordings are large GIFs in
[`assets/animations/`](https://github.com/boersmamarcel/chatty2/tree/main/assets/animations)
(`file_add_edit_delete.gif`, `shell_command.gif`, `mcp_add_edit_delete2.gif`) —
open them on GitHub rather than inlining them here.

## Built-in tools

Approval `✓` means the call can be gated by the approval mode.

### Filesystem & code

| Tool | What the agent can do | Approval |
|------|----------------------|:--------:|
| `read_file` | Read any text file in the workspace | — |
| `read_binary` | Read binary files as base64 | — |
| `list_directory` | List directory contents with metadata | — |
| `glob_search` | Find files (`**/*.rs`, `src/**/*.test.js`, …) | — |
| `write_file` | Create or overwrite files | ✓ |
| `apply_diff` | Apply unified diffs to existing files | ✓ |
| `create_directory` | Create directories | ✓ |
| `delete_file` | Delete files or directories | ✓ |
| `move_file` | Move or rename files | ✓ |

### Shell & code execution

| Tool | What the agent can do | Approval |
|------|----------------------|:--------:|
| `shell_execute` | Sandboxed shell with streaming output | ✓ |
| `shell_cd` / `shell_set_env` / `shell_status` | Working directory, env, session status | — |
| `execute_code` | Python (MontySandbox or Docker), JS, TS, Rust, or Bash | ✓ |

Git (`git_status`, `git_diff`, `git_commit`, …) and code search (`search_code`,
`find_files`, `find_definition`) are in the [tools catalog](../dev/reference/tools-catalog.md).

### Data & documents

| Tool | What the agent can do | Approval |
|------|----------------------|:--------:|
| `query_data` | SQL over Parquet, CSV, JSON via DuckDB | — |
| `describe_data` | Schema of Parquet, CSV, or JSON | — |
| `read_excel` / `write_excel` / `edit_excel` | Spreadsheets | write/edit ✓ |
| `read_docx` / `write_docx` | Word documents | write ✓ |
| `read_pptx` / `write_pptx` | PowerPoint | write ✓ |
| `pdf_to_image` / `pdf_info` / `pdf_extract_text` | PDF preview, metadata, text | — |
| `compile_typst` | Typst markup → PDF | ✓ |
| `doc_retriever` | BM25 search over workspace docs and source | — |

### Visuals, web, memory, agents

| Tool | What the agent can do | Approval |
|------|----------------------|:--------:|
| `add_attachment` | Show a generated image or PDF in chat | — |
| `create_chart` | Bar, line, pie, donut, area, candlestick | — |
| `search_web` | Tavily / Brave / DuckDuckGo lite fallback | — |
| `fetch` | Fetch a URL as readable text | — |
| `browser_navigate` / `browser_snapshot` / `browser_screenshot` | Built-in browser: open a local page, read its structure, capture it | — |
| `browser_console` / `browser_network` | Errors and failed requests from the page | — |
| `browser_resize` | Check responsive behaviour at a given viewport | — |
| `browser_use` | [browser-use](https://browser-use.com) cloud browser agent | — |
| `daytona_run` | Isolated [Daytona](https://app.daytona.io) cloud sandbox | ✓ |
| `remember` / `search_memory` | Persistent memory | — |
| `save_skill` / `read_skill` | Named procedures | — |
| `list_tools` | List tools and schemas | — |
| `list_agents` / `invoke_agent` | Discover and call configured agents | — |
| `ask_user` | Pause and ask up to 4 clarifying questions, each with pre-made options plus a free-text answer | — |
| `write_todos` / `update_todo` / `verify_completion` | Plan + verify | — |
| `sub_agent` | Headless `chatty-tui` child agent | ✓ |
| `publish_wasm_module` | Publish WASM to Hive (when Hive MCP is configured) | ✓ |

`search_web`, `fetch`, `browser_use`, and `daytona_run` require **Internet
Access** in Settings → Search. `browser_use` and `daytona_run` also need API
keys in that page's External Services section. Set the key to activate; use
the toggle to disable without deleting the key.

`ask_user` shows up as a card above the chat input; pick an option or type
your own answer, then submit. Questions you leave blank are sent as
unanswered so the agent knows what it still doesn't know. In the terminal
interface it replaces the input row instead — see
[Terminal interface](./terminal.md).

### The built-in browser

The `browser_*` tools drive a real Chrome on your machine, so the agent can look
at what it just built instead of guessing: it renders the page, screenshots it,
spots the problem, fixes it, and re-checks.

One limit worth knowing: the screenshot reaches the model on its **next** turn,
not inside the tool result. None of the providers Chatty supports (OpenRouter,
Ollama, Azure OpenAI) accept images in tool results, so the image is attached the
same way a chart or rendered PDF page is. In practice the agent captures the
screenshot, finishes its turn, and reviews the image on the turn after. Say
"keep going" if it stops after capturing.

By default they are limited to **`localhost` and `file://` URLs inside your
workspace** — this is for reviewing your own work, not for browsing the web.
Turn on **Internet Access** in Settings → Search and the browser can also open
public websites, using the same address filtering as `fetch` and `search_web`
to keep private/internal network targets out of reach. The browser profile is
never signed in to anything, so none of these tools asks for approval either way.

Turn them on with **Enable Browser Tools** in Settings → Code Execution. They
need a workspace directory, which is where screenshots and console logs are
written (`.chatty/browser/`).

Chrome is not bundled with Chatty. If you already have Chrome, Chromium, or Edge
installed, that is used. Otherwise the first browser tool call downloads a pinned
[Chrome for Testing](https://developer.chrome.com/blog/chrome-for-testing) build
— roughly 190MB, once — and verifies it before use. Expect the first call to take
a minute; later ones start immediately.

## Extensions & MCP

**Settings → Extensions** lists MCP servers, A2A agents, and WASM modules.

**Hive Marketplace** — search and install community extensions. Installed
items show type (**Agent**, **MCP**, **A2A**) and context:

| Badge | Meaning |
|-------|---------|
| **• Local** | WASM module on this machine |
| **☁ Cloud** / **☁ Cloud Only** | Hive runner; cloud-only modules have no local toggle |
| **↗ External** | MCP or A2A at an external URL |
| **Paid** | Non-free pricing model |

WASM modules that support both modes get **Switch to Local** / **Switch to Cloud**.

### Build your own WASM module

Developer guide:
[Build a WASM plugin](../dev/guides/build-wasm-module.md)
with tutorials for the reference modules
[echo-agent](../dev/guides/tutorial-echo-agent.md) and
[benford-agent](../dev/guides/tutorial-benford-agent.md).
Source lives under
[`modules/`](https://github.com/boersmamarcel/chatty2/tree/main/modules)
in the repository.

### Built-in catalog

Pre-loaded under **Installed**, disabled until you click **Enable**:

| Integration | What it provides | Auth |
|-------------|------------------|------|
| **Hugging Face** | Hub models, datasets, Spaces | Optional token |
| **Notion** | Pages, databases, comments | OAuth |
| **Atlassian** | Jira issues, Confluence pages | OAuth |

Notion and Atlassian speak SSE. Chatty's MCP client is streamable HTTP;
those endpoints may need an SSE bridge until native SSE lands. Developer
notes: [Curated MCP catalog](../dev/architecture/curated-mcp-catalog.md).

### Add a custom MCP server

1. Start the MCP server yourself (Chatty connects; it does not launch it)
2. **Settings → Extensions → Add Custom Extension → Add MCP Server**
3. Enter the **URL** and optional **API key** (`Authorization: Bearer <key>`)

The agent can list servers at runtime via `list_mcp_services`. Keys are
masked (`****`); the model never sees the real value.

### Servers that pair well

Start the process, then add its URL in Extensions. Examples:

| Server | Typical start |
|--------|----------------|
| GitHub | `npx -y @modelcontextprotocol/server-github` (`GITHUB_TOKEN`) |
| Filesystem | `npx -y @modelcontextprotocol/server-filesystem /path/to/dir` |
| PostgreSQL | `npx -y @modelcontextprotocol/server-postgres` (`POSTGRES_CONNECTION_STRING`) |
| Brave Search | `npx -y @modelcontextprotocol/server-brave-search` (`BRAVE_API_KEY`) |
| Memory | `npx -y @modelcontextprotocol/server-memory` |
| Puppeteer | `npx -y @modelcontextprotocol/server-puppeteer` |
| Fetch | `npx -y @modelcontextprotocol/server-fetch` |
| Hugging Face (hosted) | URL `https://huggingface.co/mcp` + access token |

Also preconfigured (disabled by default): Atlassian, Google Calendar, Gmail,
Google Drive. Enabling them starts the provider OAuth flow; tokens stay local.

Write your own against the [MCP specification](https://modelcontextprotocol.io/).

## Security notes

- MCP env vars sent to the LLM are **masked** (`****` sentinel)
- Side-effect tools (writes, shell, sub-agents) respect approval mode
- Workspace path restricts filesystem access

See [Security & sandboxing](./security.md).
