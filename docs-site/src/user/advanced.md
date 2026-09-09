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
| Settings, provider keys, extensions | `~/Library/Application Support/chatty/` | `~/.config/chatty/` | `%APPDATA%\chatty\` |
| Conversations (`conversations.db`) | same folder | `~/.config/chatty/` | same folder |
| Training exports | `…/chatty/exports/` | `~/.config/chatty/exports/` | `…\chatty\exports\` |
| Catalog extension sign-in tokens | same folder | `~/.config/chatty/` | same folder |
| Memory (`memory.mv2`) | same folder | `~/.local/share/chatty/` | same folder |
| Global skills | `…/chatty/skills/` | `~/.local/share/chatty/skills/` | `…\chatty\skills\` |
| Installed modules | `…/chatty/modules/` | `~/.local/share/chatty/modules/` | `…\chatty\modules\` |
| Downloaded Chrome builds | `…/chatty/browsers/<version>/` | `~/.local/share/chatty/browsers/<version>/` | `…\chatty\browsers\<version>\` |
| PDF rendering library cache | `…/chatty/lib/` | `~/.local/share/chatty/lib/` | `…\chatty\lib\` |
| Rendered math and diagram caches | `…/chatty/math_cache/`, `…/chatty/mermaid_cache/` | `~/.config/chatty/math_cache/`, `~/.config/chatty/mermaid_cache/` | `…\chatty\math_cache\`, `…\chatty\mermaid_cache\` |
| Browser screenshots and console logs | `<workspace>/.chatty/browser/` | same | same |
| Workspace skills | `<workspace>/.claude/skills/` | same | same |
| Terminal app binary | `/usr/local/bin/chatty-tui` | `~/.local/bin/chatty-tui` | the app's install folder |

The caches can be deleted at any time; they are rebuilt on demand (the first browser call after deleting the Chrome cache downloads it again). Settings and conversations are plain files, so back them up by copying the folder.

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
