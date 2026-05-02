# Curated MCP server catalog

Chatty ships with a small built-in catalog of well-known external [Model Context
Protocol](https://modelcontextprotocol.io/) servers. Each curated entry is
seeded into the user's MCP configuration on first launch as **disabled**, so
users opt in explicitly for every integration.

Once seeded, a curated entry behaves exactly like any other MCP server: it can
be enabled/disabled, edited, or deleted from **Settings → Extensions**, and its
state (and any cached credentials/tokens) are persisted across restarts.

The catalog is defined in
[`crates/chatty-core/src/install.rs`](../crates/chatty-core/src/install.rs)
(`CURATED_MCP_SERVERS`). To propose a new entry, add a `CuratedMcpServer`
record there — no other wiring is required.

## Catalog entries

### Notion

| Field | Value |
|:------|:------|
| Extension ID | `mcp-notion` |
| Server name | `notion` |
| Endpoint | `https://mcp.notion.com/sse` (SSE transport) |
| Docs | <https://developers.notion.com/docs/mcp> |
| Auth | OAuth (browser-based, one-click) |
| Default state | Disabled |

**Setup**

1. In **Settings → Extensions**, enable the **Notion** entry.
2. On first connect, Chatty discovers Notion's OAuth metadata from the server's
   `.well-known/oauth-protected-resource` endpoint and opens Notion's
   authorization page in your default browser.
3. Approve the integration for the Notion workspace(s) you want Chatty to
   access. Notion's authorization screen lets you scope access per workspace
   and per page/database.
4. After approval, Chatty caches the resulting OAuth tokens locally and the
   server's auth status switches to `Authenticated`.

**Failure states**

The auth status surfaces the following conditions in the Extensions UI:

- `NeedsAuth` — Notion requires OAuth but no cached tokens exist (initial
  state, or after tokens are revoked from the Notion side).
- `Connecting` — OAuth flow or connection is in progress.
- `Failed(reason)` — connection failed. Common causes:
  - The browser-based OAuth flow was cancelled or timed out.
  - The Notion workspace administrator has not granted the integration access,
    or the integration was revoked.
  - The host running Chatty has no outbound network access to
    `mcp.notion.com`.

A failed connection can be retried at any time by toggling the extension off
and on, which clears the runtime auth status and re-runs the OAuth probe.

**Notes**

- Notion's hosted MCP server uses the SSE transport. No API key is configured
  on the entry — authentication is handled entirely through the OAuth flow,
  not via the `Authorization: Bearer` header.
- The OAuth tokens are stored in Chatty's local credential cache, not in
  `extensions.json`, and are never sent to the LLM.
