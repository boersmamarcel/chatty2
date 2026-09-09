# Extensions

**When to read this:** You want to give the agent more reach — MCP servers, remote agents and modules from the Hive marketplace, the built-in catalog, or a server of your own.

**Settings → Extensions** is one page with four parts: your Hive account, what is installed, the marketplace, and a form for custom servers. Enabled extensions show up on the new-conversation start screen and their tools become available on the next message.

## Hive marketplace

Hive is the registry Chatty installs extensions from; the address it is talking to is shown next to **Hive Account**. **Sign In** or **Register** (username, email, password) to install from it and to run cloud modules; the session lasts thirty days.

**Browse Marketplace** searches the registry. Each result shows its type and pricing; **Install** adds it to your **Installed** list, **Uninstall** removes it. Installed items carry two kinds of badge:

| Badge | Meaning |
|-------|---------|
| **MCP** / **Agent** / **A2A** | What kind of extension it is — a tool server, a local module, or a remote agent |
| **• Local** | A module that runs on this machine |
| **☁ Cloud** / **☁ Cloud Only** | Runs on the Hive runner; cloud-only modules cannot be switched local |
| **↗ External** | An MCP server or agent at an external URL |
| **Paid** | Not free — check the pricing before enabling |

Each row has **Enable** / **Disable** (🟢 enabled, ⏸ disabled), and modules that support both modes offer **Switch to Local** / **Switch to Cloud**.

## Built-in catalog

A handful of well-known services are pre-loaded under **Installed**, disabled until you click **Enable**:

| Integration | What the agent gets | Sign-in |
|-------------|--------------------|---------|
| **Hugging Face** | Hub models, datasets and Spaces | Optional — add an access token as the API key for private repositories |
| **Notion** | Pages, databases and comments | Sign in to your Notion workspace when prompted |
| **Atlassian (Jira + Confluence)** | Issues, comments and Confluence pages | Atlassian Cloud sign-in in the browser on first connect |
| **Google Calendar**, **Gmail**, **Google Drive** | Events, mail, files | Sign in with your Google account when prompted |

Sign-in tokens stay on your machine. The maintained list, with each service's connection notes, is on the [curated MCP catalog](../dev/architecture/curated-mcp-catalog.md) page.

## Add a custom MCP server

Chatty connects to servers you run; it does not launch them.

1. Start the server yourself and note its URL.
2. **Add Custom Extension → Add MCP Server**.
3. Give it a name (for example `github-mcp`), the URL (for example `http://localhost:3000/mcp`) and an optional API key, then **Add**.

The server appears under **Installed** with an **↗ External** badge. The agent can list your servers at runtime, but any key you enter is masked — the model never sees the real value ([Security & sandboxing](./security.md)).

> [!TIP]
> Servers to try, with their start commands, are collected on the [curated MCP catalog](../dev/architecture/curated-mcp-catalog.md) page. Write your own against the [MCP specification](https://modelcontextprotocol.io/).

## Build your own module

Modules are small programs that run inside Chatty, locally or on the Hive runner. The developer guide [Build a WASM module](../dev/guides/build-wasm-module.md) walks through it, with two worked examples: [echo-agent](../dev/start/tutorial-echo-agent.md) and [benford-agent](../dev/start/tutorial-benford-agent.md).

## Next

- [Agents & tools](./agents-and-tools.md)
- [Security & sandboxing](./security.md)
- [Sub-agents](./sub-agents.md)
