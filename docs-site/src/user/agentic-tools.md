# Agentic tools

**When to read this:** Enable filesystem, shell, MCP, and sub-agent capabilities.

## Enable in Settings

1. **Settings → Code Execution**
2. Set **workspace directory** (absolute path)
3. Toggle **code execution** on
4. Choose **approval mode**: ask every time / auto-approve / deny all

## Tool categories

See the full [tools catalog](../dev/reference/tools-catalog.md).

| Category | Examples |
|----------|----------|
| Filesystem | `read_file`, `write_file`, `glob_search` |
| Shell | `shell_execute`, `shell_cd` |
| Git | `git_status`, `git_commit`, … |
| MCP | `list_mcp_services` + configured servers |
| Memory | `remember`, `search_memory`, `save_skill` |
| Agents | `sub_agent`, `invoke_agent`, `list_agents` |
| Sandbox | `execute_code`, `daytona_run` |

## Slash commands

Type `/` in the chat input for `/clear`, `/compact`, `/context`, `/cwd`, etc.
See [slash commands reference](../dev/reference/slash-commands.md).

## Security

- MCP env vars sent to the LLM are **masked** (`****` sentinel)
- Shell and write tools respect approval stores
- Workspace path restricts filesystem access
