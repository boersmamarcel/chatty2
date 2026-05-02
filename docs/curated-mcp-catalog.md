# Curated MCP server catalog

Chatty ships with a small, hand-picked catalog of well-known external MCP
servers so common integrations are one click away. The catalog lives in
[`crates/chatty-core/src/curated_mcp.rs`](../crates/chatty-core/src/curated_mcp.rs)
and is seeded into the Extensions store on first launch.

## What's in the catalog

| Provider                       | Endpoint                          | Transport | Docs                                                       |
|:-------------------------------|:----------------------------------|:----------|:-----------------------------------------------------------|
| Hugging Face                   | `https://hf.co/mcp`               | HTTP      | <https://huggingface.co/docs/hub/agents-mcp>               |
| Notion                         | `https://mcp.notion.com/sse`      | SSE       | <https://developers.notion.com/docs/mcp>                   |
| Atlassian (Jira + Confluence)  | `https://mcp.atlassian.com/v1/sse`| SSE       | <https://www.atlassian.com/platform/remote-mcp-server>     |

Every catalog entry is added with `enabled = false` — users opt in
explicitly from **Settings → Extensions → Installed**.

## How users manage it

1. Open **Settings → Extensions**.
2. The curated entries appear under **Installed** with a `MCP` badge and an
   `↗ External` badge (since they're hosted by the provider).
3. Click **Enable** on the entries you want. Chatty connects in the
   background and surfaces auth / connection failures inline.
4. Click **Disable** to disconnect; the entry stays in the list so you can
   re-enable it later.
5. The enabled / disabled state is persisted to `extensions.json` and
   `mcp_servers.json`, so it survives restarts.

## Authentication

| Provider     | How to authenticate                                                                                      |
|:-------------|:---------------------------------------------------------------------------------------------------------|
| Hugging Face | Optional. Paste a Hugging Face access token into the API key field for private repos / higher rate limits. |
| Notion       | OAuth — sign in with your Notion workspace when prompted by the MCP server.                              |
| Atlassian    | OAuth — Atlassian Cloud sign-in is performed in the browser on first connect.                            |

## Caveats

- **SSE transport** — Notion and Atlassian advertise Server-Sent Events.
  The built-in MCP client speaks streamable HTTP, so connecting to those
  endpoints currently requires an SSE-capable transport bridge or proxy.
  This caveat is captured in each entry's `auth_notes` and on the
  connection error displayed in the UI when a direct connect fails.
- **Provider-specific quirks** (custom OAuth scopes, regional endpoints,
  rate limits, …) are intentionally tracked in separate per-provider
  follow-up issues so this shared catalog stays focused on the data model
  and UX.

## Adding a provider to the catalog

1. Append a new `CuratedMcpEntry { … }` constant in
   `crates/chatty-core/src/curated_mcp.rs`.
2. Make sure the `id` is unique and prefixed with `mcp-` (so it cannot
   collide with WASM module ids or A2A agent ids).
3. Add a row to the table above and document any auth quirks.
4. Update the unit tests in `curated_mcp.rs` if the new entry exercises
   metadata that's not yet covered.

The catalog is seeded by `ensure_curated_mcp_servers()`, which is
idempotent — adding a new entry simply means existing installs pick it up
on next launch without disturbing already-toggled entries.
