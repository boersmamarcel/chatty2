# Advanced

**When to read this:** You want to export conversations for fine-tuning, find where Chatty keeps its files, or understand how updates are installed.

## Training-data export

**Settings → Training Data** turns on automatic export: with **Auto-export ATIF** or **Auto-export JSONL** on, every finished reply re-exports its conversation. Re-exporting replaces that conversation's previous entry rather than duplicating it.

**ATIF** (Agent Trajectory Interchange Format) is structured JSON for agent pipelines, following the [Harbor trajectory format](https://harborframework.com/docs/agents/trajectory-format): messages, tool calls, reasoning, timestamps, token counts, thumbs feedback and regeneration pairs (rejected versus chosen). One file per conversation.

**JSONL** produces two append-only files:

- `sft.jsonl` — supervised fine-tuning rows in ChatML, as accepted by OpenAI, Anthropic, Together AI and similar APIs. Tool calls can be included.
- `dpo.jsonl` — preference pairs. Every time you press **Regenerate**, the original reply becomes the rejected answer and the new one the chosen answer.

Both land in the `exports` folder listed below.

## Where Chatty stores data

Nothing leaves your machine unless a provider, extension or website you use receives it. Windows keeps everything under `%APPDATA%\chatty\`; macOS under `~/Library/Application Support/chatty/`; Linux splits configuration from data as shown (respecting `$XDG_CONFIG_HOME` / `$XDG_DATA_HOME`).

| What | macOS | Linux | Windows |
|------|-------|-------|---------|
| Settings, extensions | `~/Library/Application Support/chatty/` | `~/.config/chatty/` | `%APPDATA%\chatty\` |
| Provider API keys, MCP and A2A tokens, Hive sign-in | Keychain (login keychain) | Secret Service (GNOME Keyring, KWallet) | Credential Manager |
| Conversations (`conversations.db`) | same folder | `~/.config/chatty/` | same folder |
| Training exports | `~/Library/Application Support/chatty/exports/` | `~/.config/chatty/exports/` | `%APPDATA%\chatty\exports\` |
| Catalog extension sign-in tokens | same folder | `~/.config/chatty/` | same folder |
| Memory (`memory.mv2`) | same folder | `~/.local/share/chatty/` | same folder |
| Global skills (see [Memory & skills](./memory-and-skills.md#skills)) | `~/.agents/skills/`, `~/.claude/skills/` | same | `%USERPROFILE%\.agents\skills\`, `%USERPROFILE%\.claude\skills\` |
| Installed modules | `~/Library/Application Support/chatty/modules/` | `~/.local/share/chatty/modules/` | `%APPDATA%\chatty\modules\` |
| Downloaded Chrome builds | `~/Library/Application Support/chatty/browsers/<version>/` | `~/.local/share/chatty/browsers/<version>/` | `%APPDATA%\chatty\browsers\<version>\` |
| PDF rendering library cache | `~/Library/Application Support/chatty/lib/` | `~/.local/share/chatty/lib/` | `%APPDATA%\chatty\lib\` |
| Rendered math and diagram caches | `~/Library/Application Support/chatty/math_cache/`, `~/Library/Application Support/chatty/mermaid_cache/` | `~/.config/chatty/math_cache/`, `~/.config/chatty/mermaid_cache/` | `%APPDATA%\chatty\math_cache\`, `%APPDATA%\chatty\mermaid_cache\` |
| Browser screenshots and console logs | `<workspace>/.chatty/browser/` | same | same |
| Project skills | `<workspace>/.agents/skills/`, `<workspace>/.claude/skills/` (and in parent folders up to the git root) | same | same |
| Sub-agent worktrees (one per delegated worker of a team with `"isolate": true`, left in place; delete when done) | `<workspace>/.chatty/worktrees/<name>/` | same | same |
| Team directories (`<id>/team.json` + `SKILL.md`) | `<workspace>/.chatty/teams/`, then `~/Library/Application Support/chatty/teams/` | same, then `~/.local/share/chatty/teams/` | same, then `%APPDATA%\chatty\teams\` |
| Context-window thresholds (`token_tracking.json`) | same folder as settings | `~/.config/chatty/` | same folder |
| Terminal app binary | `/usr/local/bin/chatty-tui` | `~/.local/bin/chatty-tui` | the app's install folder |

The caches can be deleted at any time; they are rebuilt on demand (the first browser call after deleting the Chrome cache downloads it again). Settings and conversations are plain files, so back them up by copying the folder.

### API keys and sign-in tokens

Provider API keys, MCP and A2A tokens and your Hive sign-in are kept in the operating system's keychain, filed under `chatty:<settings folder>`. The settings files only name them (`"api_key_ref": "provider/OpenRouter"`), so copying or syncing the settings folder does not copy a key. On a new machine, enter the keys again in **Settings**.

When you first start this version, any key still written in `providers.json`, `mcp_servers.json`, `a2a_agents.json` or `hive_settings.json` is moved into the keychain and removed from the file.

A Linux machine without a Secret Service (a server, a container) has no keychain. Chatty then keeps the keys in `secrets.json` in the settings folder, readable only by you, and logs one warning saying so. To control this, set `CHATTY_SECRET_STORE`:

- `keychain` — use the keychain or refuse to start; never write `secrets.json`.
- `file` — always use `secrets.json`, without touching the keychain.

Environment variables and command-line flags that pass a key to `chatty-tui` (for example `--api-key`) work as before and are never stored.

## Auto-update

Chatty checks GitHub Releases shortly after launch and then hourly. When a newer version exists, it downloads in the background, verifies the file's SHA-256 checksum, and the status footer changes to **Click to restart and install v…**. A **Check for updates** button in the same footer runs a check on demand.

Installing differs by platform:

- **macOS** — the app quits, the bundle is replaced in place and the app relaunches.
- **Linux** — the running AppImage is replaced and relaunched; a `chatty-tui` installed from the desktop app is refreshed on the next launch.
- **Windows** — the installer runs silently and restarts the app.

If a download fails its checksum it is discarded and the footer shows **Update failed**; nothing is installed.

## Next

- [Chatting](./chatting.md)
- [Terminal interface](./terminal.md)
- [Security & sandboxing](./security.md)
