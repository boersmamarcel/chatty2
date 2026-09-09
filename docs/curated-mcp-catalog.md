# Curated MCP server catalog

**When to read this:** You want to know which external MCP servers Chatty ships
pre-configured, how they are seeded and persisted, or how to add one.

Chatty ships a small, hand-picked catalog of well-known hosted MCP servers so common
integrations are one click away. The catalog is compiled in as `CURATED_CATALOG` in
[`crates/chatty-core/src/curated_mcp.rs`](https://github.com/boersmamarcel/chatty2/blob/main/crates/chatty-core/src/curated_mcp.rs)
and seeded into the Extensions store at launch.

## What's in the catalog

| Provider | Endpoint | Transport | Docs |
|:---------|:---------|:----------|:-----|
| Hugging Face | `https://hf.co/mcp` | Streamable HTTP | <https://huggingface.co/docs/hub/agents-mcp> |
| Notion | `https://mcp.notion.com/sse` | SSE | <https://developers.notion.com/docs/mcp> |
| Atlassian (Jira + Confluence) | `https://mcp.atlassian.com/v1/sse` | SSE | <https://www.atlassian.com/platform/remote-mcp-server> |
| Google Calendar | `https://calendarmcp.googleapis.com/mcp/v1` | Streamable HTTP | <https://developers.google.com/calendar> |
| Gmail | `https://gmailmcp.googleapis.com/mcp/v1` | Streamable HTTP | <https://developers.google.com/gmail> |
| Google Drive | `https://drivemcp.googleapis.com/mcp/v1` | Streamable HTTP | <https://developers.google.com/drive> |

Every entry has `default_enabled: false` — users opt in explicitly from
**Settings → Extensions → Installed**.

## How users manage it

1. Open **Settings → Extensions**.
2. The curated entries appear under **Installed** with an `MCP` badge and an
   `↗ External` badge (they are hosted by the provider, not run locally).
3. Click **Enable** on the entries you want. Chatty connects in the background and
   shows each entry's connection status inline.
4. Click **Disable** to disconnect; the entry stays in the list so you can re-enable
   it later.
5. The enabled / disabled state is persisted to `extensions.json` and
   `mcp_servers.json`, so it survives restarts.

## Authentication

| Provider | How to authenticate |
|:---------|:--------------------|
| Hugging Face | Optional. Paste a Hugging Face access token into the API key field for private repos / higher rate limits. |
| Notion | OAuth — sign in with your Notion workspace when prompted by the MCP server. |
| Atlassian | OAuth — Atlassian Cloud sign-in is performed in the browser on first connect. |
| Google Calendar / Gmail / Drive | OAuth — sign in with your Google account when prompted; grant the matching API scope. |

## Transport caveat

The built-in MCP client (`crates/chatty-core/src/services/mcp_service.rs`) speaks
**streamable HTTP** only, via rmcp's `StreamableHttpClientTransport`. Notion and
Atlassian advertise **Server-Sent Events**, so connecting to those two endpoints
requires an SSE-capable transport bridge or proxy. The `transport` field on each
`CuratedMcpEntry` records what the upstream serves, and the SSE caveat is repeated in
those two entries' `auth_notes`; the UI shows only the generic connection error when
a direct connect fails, so this page is where the explanation lives.

## Community servers that pair well

These are not in the catalog, but are common companions. The reference servers below
are **stdio** processes launched with `npx`, and `McpServerConfig` has only a `url`
(plus optional API key) — Chatty connects to an already-running streamable-HTTP
endpoint and never spawns a server itself. To use one, run it behind a local
stdio-to-HTTP bridge (for example `mcp-proxy` or `supergateway`) and add the bridge's
URL under **Settings → Extensions**. Required environment variables are set on the
bridged process, not in Chatty.

| Server | Command | Env |
|:-------|:--------|:----|
| GitHub | `npx -y @modelcontextprotocol/server-github` | `GITHUB_TOKEN` |
| Filesystem | `npx -y @modelcontextprotocol/server-filesystem /path/to/dir` | — |
| PostgreSQL | `npx -y @modelcontextprotocol/server-postgres` | `POSTGRES_CONNECTION_STRING` |
| Brave Search | `npx -y @modelcontextprotocol/server-brave-search` | `BRAVE_API_KEY` |
| Memory | `npx -y @modelcontextprotocol/server-memory` | — |
| Puppeteer | `npx -y @modelcontextprotocol/server-puppeteer` | — |
| Fetch | `npx -y @modelcontextprotocol/server-fetch` | — |
| Hugging Face (hosted) | URL `https://huggingface.co/mcp` | Access token as the API key |

The hosted Hugging Face server is already the catalog's `mcp-huggingface` entry and
needs no bridge; add it by URL only if you want a second configuration (for example a
different token).

## Adding a provider to the catalog

1. Append a `CuratedMcpEntry { … }` to `CURATED_CATALOG` in
   `crates/chatty-core/src/curated_mcp.rs`. Every field is `'static`: `id`, `slug`,
   `display_name`, `url`, `transport`, `description`, `docs_url`, `auth_notes`,
   `default_enabled`.
2. Make the `id` unique and prefixed with `mcp-` so it cannot collide with WASM module
   ids or A2A agent ids; the `slug` becomes the MCP server's `name`.
3. Add a row to the table above and document any auth quirks.
4. Extend the unit tests in `curated_mcp.rs` (`catalog_contains_initial_providers`
   asserts every id).

The catalog is seeded by `ensure_curated_mcp_servers(extensions, mcp_servers)`, called
from `main.rs` after the extension and MCP stores have loaded. It is idempotent —
entries already installed (matched by `id`) are left alone, so a user's `enabled` flag
or API key is never overwritten — and returns `true` when it added something, which is
the caller's cue to persist both stores. Adding a new entry therefore reaches existing
installs on their next launch without disturbing already-toggled entries.
