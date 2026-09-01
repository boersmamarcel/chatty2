# Agentic tools

Give the model filesystem, shell, MCP, and sub-agent access. Tools stay off
until you enable them.

The full name-by-name list is the
[tools catalog](../dev/reference/tools-catalog.md).

## Enable in Settings

1. **Settings → Code Execution**
2. Set a **workspace directory** (absolute path)
3. Turn **code execution** on
4. Choose **approval mode**: ask every time / auto-approve / deny all

Internet tools have a separate toggle in **Settings → Search**.

![Internet access settings](./img/advanced_internet_access_settings.gif)

## What the agent can do

| Category | Examples |
|----------|----------|
| Filesystem | `read_file`, `write_file`, `apply_diff`, `glob_search` |
| Shell | `shell_execute`, `shell_cd` |
| Code | `execute_code` (MontySandbox Python; Docker fallback for JS/TS/Rust/Bash and third-party packages) |
| Git | `git_status`, `git_commit`, … |
| Data | `query_data`, Excel / Word / PowerPoint / PDF tools |
| Web | `search_web`, `fetch`, `browser_use` (API key in Settings → Search) |
| Memory | `remember`, `search_memory`, `save_skill` |
| Agents | `sub_agent`, `invoke_agent`, `list_agents` |
| Sandbox | `daytona_run` |

![File operations](./img/file_add_edit_delete.gif)

![Shell command execution](./img/shell_command.gif)

![Web fetch](./img/webfetch.gif)

## MCP and extensions

**Settings → Extensions** lists MCP servers, A2A agents, and WASM modules.

- Browse the Hive marketplace to install community extensions.
- Built-in catalog entries (Hugging Face, Notion, Atlassian, Google) start
  disabled — click **Enable** to connect.
- Add a custom MCP server with **Add Custom Extension** (URL + optional
  Bearer key). Chatty connects to a server you already started; it does not
  launch the process.

The agent can call `list_mcp_services` at runtime. API keys are masked
(`****`) so the model never sees secrets.

![MCP server management](./img/mcp_add_edit_delete2.gif)

## Slash commands

Type `/` in the chat input for `/clear`, `/compact`, `/context`, `/cwd`,
`/agent`, and others. See the
[slash commands reference](../dev/reference/slash-commands.md).

## Security

- MCP env vars sent to the model are **masked** (`****` sentinel)
- Write, shell, and sub-agent tools go through the approval store
- The workspace path is the filesystem boundary

More detail: [Security & sandboxing](./security.md).
