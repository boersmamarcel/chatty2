# Extensions

**When to read this:** You want to give the agent more reach — MCP servers, remote agents and modules from the Hive marketplace, the built-in catalog, or a server of your own.

**Settings → Extensions** is one page with four parts: your Hive account, what is installed, the marketplace, and a form for custom servers. Plugins — WASM modules, from Hive or copied in by hand — are listed on their own page, **Settings → Plugins**. Enabled extensions show up on the new-conversation start screen and their tools become available on the next message.

## Hive marketplace

Hive is the registry Chatty installs extensions from; the address it is talking to is shown next to **Hive Account**. **Sign In** or **Register** (username, email, password) to install from it and to run cloud modules; the session lasts thirty days.

**Browse Marketplace** searches the registry. Each result shows its type and pricing; **Install** downloads it into your module directory (the one set in the module settings) and adds it to your **Installed** list, **Uninstall** removes it. Chatty refuses a module whose name or version is malformed or whose download is over 64 MiB, and remembers each installed module's checksum: if its file changes on disk afterwards, it no longer loads. Installed items carry two kinds of badge:

| Badge | Meaning |
|-------|---------|
| **MCP** / **Plugin** / **A2A** | What kind of extension it is — a tool server, a WASM plugin, or a remote agent |
| **• Local** | A module that runs on this machine |
| **☁ Cloud** / **☁ Cloud Only** | Runs on the Hive runner; cloud-only modules cannot be switched local |
| **↗ External** | An MCP server or agent at an external URL |
| **Paid** | Not free — check the pricing before enabling |
| **Trust: signed** / **Trust: local** | Where a loaded module's code comes from: signed by its publisher and installed from Hive, or copied in by hand |

**Paid** plugins are metered wherever they run, including when an agent spec lists them and when a delegated worker calls them. Each module's free calls are used first (the count comes from Hive); after that a call needs a positive credit balance, and every call is reported to Hive. If Chatty cannot read your balance — you are signed out, your sign-in has expired, or Hive is unreachable — a paid call is refused with "Cannot verify credits … sign in to Hive" rather than run unbilled. Delegated workers reuse the sign-in the desktop app saved, so if the app has been idle for over an hour, open it once to refresh the session.

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

## Remote A2A agents and private networks

A remote **A2A** agent (installed from the marketplace or added as a custom extension) is called over HTTP, the same as an MCP server. By default it can only be reached at a public address or `localhost`: a name that resolves to your LAN, your Tailscale tailnet, or any other private range is refused, so a malicious or compromised marketplace agent can't be pointed at your own network.

If you run an agent yourself on your LAN or tailnet, turn on its **Private network** toggle (next to **Enable**/**Disable** under **Installed**) to let that one agent reach a private address. It admits the same ranges as the browser's **Allow Browser Access to Private Network** toggle: RFC-1918 LAN addresses, Tailscale's CGNAT range, and IPv6 unique-local addresses. Cloud-metadata addresses (`169.254.x.x`) and IPv6 link-local addresses stay refused either way. A private-network agent still needs `https://`: plain `http://` only ever works for `localhost`.

Turn this on only for an agent whose address you control. The check runs against whatever address the agent's name resolves to at the moment of each call, not once when you add it — so if the name is ever pointed somewhere else (DNS rebinding), the opt-in lets that new address through too, private range or not.

## Build your own module

Modules are small programs that run inside Chatty, locally or on the Hive runner. The developer guide [Build a WASM module](../dev/guides/build-wasm-module.md) walks through it, with two worked examples: [write a plugin (echo)](../dev/start/tutorial-echo-agent.md) and [give an agent the plugin (benford)](../dev/start/tutorial-benford-agent.md).

A module is a **plugin**: tools an agent runs, never an agent of its own. Installing one adds it to the module directory and nothing more; an agent uses it once its spec lists the plugin under `[[plugins]]` and grants a subset of what the plugin asks for — an LLM call, a file read, billing, and so on. Anything not granted is refused to the plugin, not silently allowed. **Settings → Plugins** lists every plugin with its tools, what it requests, trust level and the agent specs that use it, each with what it actually grants; **Settings → Agents** shows the same requests-vs-grants breakdown for each spec's plugins. A module you copy into the module directory yourself shows **Trust: local**: Chatty loads it because you put it there, but nothing vouches for it.

If a module fails to load — an invalid `module.toml`, a missing `.wasm` file, a name that clashes with another installed module, or an installed `.wasm` that changed on disk since it was installed (`hash mismatch`) — its row shows **Failed to load:** with the reason, instead of failing silently. Reinstall a module that fails with a hash mismatch.

## Use a plugin from another MCP client

A plugin whose `module.toml` sets `[protocols] mcp = true` is also served to MCP clients outside Chatty, by the desktop's module gateway (**Enable module runtime** in **Settings → Plugins**). Served that way there is no agent spec to grant from, so a plugin gets only logging and config by default. If it asks for an LLM call, a file read or billing, its row in **Settings → Plugins** shows a switch for each, off until you turn it on; switching one on reloads the gateway. The gateway starts the first time an agent delegates, and **Settings → Plugins** then shows where it runs.

The gateway has no network port. It listens on a Unix socket in a folder only you can open, and answers only a caller that sends the token Chatty writes there each time it starts the gateway:

- **Socket:** `gateway.sock` in `$XDG_RUNTIME_DIR/chatty-run`. Without `XDG_RUNTIME_DIR` (macOS, some Linux setups) the folder is `chatty-run` in your cache folder: `~/Library/Caches` or `~/.cache`.
- **Token:** `gateway.token`, next to the socket, readable only by you. Send it as `Authorization: Bearer <token>`; it changes every launch, so read it from the file each time.

```sh
dir="$XDG_RUNTIME_DIR/chatty-run"
curl -s --unix-socket "$dir/gateway.sock" http://localhost/mcp/echo \
  -H "Authorization: Bearer $(cat "$dir/gateway.token")" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

A client that can only reach an HTTP URL needs a local bridge from a port to the socket. The bridge answers anyone who can reach that port, so bind it to `127.0.0.1` and still send the token.

Chatty refuses to start the gateway when the `chatty-run` folder is someone else's or can be opened by other users (anything but mode `700`); remove the folder and it is recreated correctly. On Windows the gateway does not start yet: it needs an owner-only folder there, which Chatty does not create yet, so external MCP access (reaching a plugin's tools from another MCP client) is macOS/Linux-only for now.

## Next

- [Agents & tools](./agents-and-tools.md)
- [Security & sandboxing](./security.md)
- [Sub-agents](./sub-agents.md)
