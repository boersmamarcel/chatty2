# Plugins

**When to read this:** You are writing, installing or debugging a WASM plugin, or need to
know how a plugin's tools reach an agent and where the runtime, registry and gateway
fit.

Authoring one? Start with
[Build a WASM plugin](../docs-site/src/dev/guides/build-wasm-module.md) (quick start
and host-LLM sequence diagrams); the WIT types are in [wit-reference.md](wit-reference.md).
Agents — including the specs that use plugins — are on [agents-and-specs.md](agents-and-specs.md).

## What a plugin is

A WASM module is a **plugin**: it contributes tools to a chatty agent and
never runs a loop of its own (PL-D1 option B). The agent is always an agent
spec run by chatty's harness; a spec's `[[plugins]]` load the plugins it uses,
one instance per agent, and the model sees their tools as `<plugin>__<tool>`
("A plugin's tools as the agent's own" below). A plugin is never
listed or invoked as an agent itself.

### Architecture stack

```
┌──────────────────────────────────────────────────────────┐
│                    chatty-module-sdk                      │
│  (Rust SDK for plugin authors, targets wasm32-wasip2)    │
│  Provides: Plugin trait + export! (generated from WIT),  │
│            llm::complete, config::get, log::info, etc.   │
├──────────────────────────────────────────────────────────┤
│                   chatty-wasm-runtime                     │
│  (Wasmtime-based host: loads chatty:plugin@0.3.0 .wasm,  │
│   implements imports, calls guest exports with           │
│   fuel/memory/timeout limits)                            │
├──────────────────────────────────────────────────────────┤
│                  chatty-module-registry                   │
│  (Discovers modules on disk, parses module.toml,         │
│   manages load/unload/reload lifecycle)                  │
├──────────────────────────────────────────────────────────┤
│   chatty-core (plugin_tool)   │  chatty-protocol-gateway  │
│   a spec's plugins as rig     │  the same tools to        │
│   tools of its agent          │  external MCP clients     │
└──────────────────────────────────────────────────────────┘
```

| Crate | Role |
|:------|:-----|
| `chatty-module-sdk` | Guest-side SDK for plugin authors: the WIT types, one module per host capability, and wit-bindgen's own `Plugin` trait and `export!` macro |
| `chatty-wasm-runtime` | Wasmtime host: loads `chatty:plugin@0.3.0` components (and refuses every other world), implements the host imports (`llm`, `config`, `logging`, `file`, `billing`), enforces resource limits |
| `chatty-module-registry` | Discovery (`scan_directory`), lifecycle (`load`/`unload`/`reload`), manifest parsing |
| `chatty-core` | `tools::plugin_tool` (a spec's plugins as rig tools), `PluginLlmProvider` (a plugin's `llm::complete` on the agent's provider), the A2A client and agent tools |
| `chatty-protocol-gateway` | HTTP server (axum) serving plugin tools over MCP, and local participants and virtual agents over A2A |

### WIT contract

The host–guest interface is
[`wit/chatty-plugin.wit`](https://github.com/boersmamarcel/chatty2/blob/main/wit/chatty-plugin.wit)
(package `chatty:plugin@0.3.0`); [wit-reference.md](wit-reference.md) has the full
type reference. A component targeting any other world (the old agent-shaped
`chatty:module@0.2.0` included) is refused at load with `module targets …; this
chatty supports chatty:plugin@0.3.0 — rebuild it with the current SDK`.

**Host imports**, one interface per capability (a plugin lists the ones it
needs in `metadata().requested-capabilities`; the host links only what the agent's
spec grants, and refuses the rest with `capability <x> not granted to this agent`, PL-U4):

| Interface | Function | Purpose |
|:----------|:---------|:--------|
| `llm` | `complete(model, messages, tools)` | A completion on the calling agent's model, through its provider client |
| `config` | `get(key)` | Read the manifest's `[config]` table |
| `logging` | `log(level, message)` | Structured logs to the host's `tracing` (always granted) |
| `file` | `read-bytes(path)` | Read under the manifest's `[files] root`; a plugin without `[files]` reads nothing |
| `billing` | `acquire-session(estimated_tokens)`, `report-usage(input, output)` | Paid plugins; reserves and settles credits against a Hive-signed session token |

**Guest export** `plugin`:

| Function | Purpose |
|:---------|:--------|
| `metadata() → plugin-metadata` | Name, version, description, requested capabilities, config keys |
| `list-tools() → definitions` | The tools the plugin provides |
| `invoke-tool(tool-call-request) → result<tool-result, tool-error>` | Run one tool call; a typed error (`unknown-tool`, `invalid-arguments`, `denied`, `failed`) reaches the model as `<kind>: <message>` |

### Module directory layout

```
<module dir>/
├── echo/
│   ├── module.toml          # Manifest (required)
│   ├── echo.wasm            # WASM component binary
│   └── .chatty-install.json # Install record (only for modules installed from Hive)
└── benford/
    ├── module.toml
    └── benford.wasm
```

The directory is configurable via module settings. Platform defaults:

| Platform | Path |
|:---------|:-----|
| macOS | `~/Library/Application Support/chatty/modules/` |
| Linux | `~/.local/share/chatty/modules/` (or `$XDG_DATA_HOME/chatty/modules/`) |
| Windows | `%APPDATA%\chatty\modules\` |

### Module manifest (`module.toml`)

```toml
[module]
name = "echo"
version = "0.2.0"
description = "A simple echo plugin"
wasm = "echo.wasm"              # Plain relative path inside this directory (no `..`, no absolute)
# execution_mode = "local"      # "local" | "remote" | "remote_only"; remote modules run on the hive-runner (PL-H8 retires this)

[capabilities]
tools = ["echo", "reverse"]     # Tool names the plugin exposes

[protocols]
mcp = true                      # Serve the tools to external MCP clients at /mcp/{name}

[resources]
max_memory_mb = 64              # Memory cap (0 = use default: 256 MiB; may only lower)
max_execution_ms = 30000        # Per-call timeout (0 = use default: 60 s; may only lower)

[config]                        # Optional: string → string values the guest reads via config::get
greeting = "hello"

[files]                         # Optional: the only directory file::read-bytes may read
root = "weights"                # Plain relative path inside this directory
```

Parsing is strict: an unknown key or table (including the agent-world keys
`[capabilities] chat` and `agent`, and `[protocols] openai_compat` and `a2a`, removed by
PL-U3 and PL-U5 — a plugin is never an agent), an `execution_mode` other than
`local`/`remote`/`remote_only`, a non-string `[config]` value, or a `wasm`/`[files].root`
path that is absolute or uses `..`, `\` or `:` is a manifest error. A `[resources]` value
above a host ceiling is clamped to it, with a warning on the manifest
(`ModuleManifest::warnings`). Installing from Hive writes `module.toml` from the Hive
manifest and copies only `tools` and `mcp`, so a Hive-installed module never becomes an
agent whatever its Hive manifest still says.

`ModuleRegistry::scan_directory` visits module directories in name order and returns a
`ScanReport { loaded, remote, failed }`: every directory that did not load is in `failed`
with its reason, and remote modules are listed apart from local loads. Two directories
declaring the same `name`: the first by directory name wins, the second is a failure (so is
`load` of a name already registered from another directory). The desktop's installed
extensions list shows a module's failure reason under its row.

**Install hardening (PL-H5a).** `chatty_core::install` checks a registry-supplied module
name against the registry's rule (`^[a-z][a-z0-9-]{1,48}[a-z0-9]$`, no `--`) and the
version as semver before anything touches the filesystem, installs into the configured
`module_dir`, and caps the download at `hive_client::MAX_DOWNLOAD_BYTES` (64 MiB) while it
streams. Each WASM install writes `.chatty-install.json` (`{sha256, trust_level,
publisher_key_id}`) beside the module; the registry hashes the `.wasm` bytes it is about
to compile against that record at every load and refuses a mismatch (`hash mismatch …`,
shown as `Failed to load:`). A module without a record — copied in by hand — loads as
`TrustLevel::Local` (`ModuleRegistry::trust_level`), and Settings → Plugins marks it
*copied in by hand*.

**Signed installs (PL-H5).** Every registry download is verified against a registry
root public key chatty holds, never one the registry sends (`hive_client::verify`, which
mirrors hive's `hive-verify` byte for byte; `crates/hive-client/tests/vectors/` are
hive's shared vectors). The root certifies the publisher's key, the publisher's key signs
a canonical manifest `{capabilities, name, sha256, version, wit_version}`, and the
manifest's `sha256` must match the downloaded bytes and its `name`/`version` the module
asked for. A download missing any of the four `X-Hive-*` chain headers, or failing any
link, is refused; `module.toml` takes its tools from the signed capabilities. Which root
key is trusted (`hive_client::trust`): the compiled production key
(`PRODUCTION_ROOT_PUBLIC_KEY`, empty until pinned, so the production registry is refused
until then), or `CHATTY_HIVE_ROOT_KEY` for a **local** (loopback) registry such as the
compose stack; the override is ignored for any other host. A registry with no trusted
root refuses every download before the request (no trust on first use).

### Resource limits

Every plugin runs inside a sandboxed Wasmtime instance
(`crates/chatty-wasm-runtime/src/limits.rs`). Every limit is **per call**: fuel is
refilled and the deadline re-armed before each export call. The defaults are the host
ceilings; a manifest's `[resources]` may only lower them — a larger value is clamped
down to the ceiling.

| Limit | Default = ceiling | Enforcement | Error |
|:------|:------------------|:------------|:------|
| **Fuel** | 10¹² units per call | Wasmtime fuel (≈1 unit per Wasm instruction) | `fuel exhausted` |
| **Wall clock** | 60 s per call, host time included | Epoch interruption (10 ms ticks); host imports (`llm::complete`, `file::read-bytes`, billing) stop waiting at the deadline | `deadline exceeded` |
| **Memory** | 256 MiB | Store memory limiter | `memory limit` |
| **Output** | 1 MiB per call | Size of each export's return value | `output too large` |

The fuel ceiling (AGE-708) is sized so a pure CPU-bound guest is bounded by the 60 s
wall-clock ceiling, not by fuel: on this host, Wasmtime fuel runs at roughly
1.5 × 10¹⁰ units/s for a tight arithmetic loop, so 10⁹ (PL-D3's original figure) was
exhausted in well under a second — 10¹² keeps a pure spin running for over a minute of
fuel, past the 60 s wall clock.

`metadata` and `list-tools` get a 1 s wall-clock budget. A guest trap or panic
fails the call with `guest trap: <message>` (the panic message is read from the guest's
stderr) and never takes the host down; the trapped instance is dropped and the plugin
re-instantiated on its next call, so guest statics start over. Callers can match the
kind with `err.downcast_ref::<chatty_wasm_runtime::CallError>()`; the guest's own
`tool-error` is a `ToolFailure` (`<kind>: <message>`).

## A plugin's tools as the agent's own

Each `[[plugins]]`
entry is loaded when the agent is built: the module directory (`module_settings.module_dir`)
is searched for a `module.toml` whose `[module].name` is `module`, its version is checked
against `version`, and one instance is made for this agent, with the module's `[config]`
(the spec's `config` on top) and `[files].root`, and its `[resources]` lowered by the
spec's `limits`. Every tool its `list-tools` names is registered next to the native tools
as `<module>__<tool>` — `echo__reverse` — because OpenAI-wire providers (OpenRouter,
Azure) refuse any tool name outside `^[a-zA-Z0-9_-]{1,64}$`, so the dotted form is only
what the transcript shows ("Ran echo.reverse"). A call goes straight into the
instance under PL-H1's per-call limits — about 40 µs, against about 1 ms through the
gateway's `/mcp/{module}` — and a trap, deadline or guest error comes back to the model as
the tool's error, with its reason, and the turn goes on. The spec is the plugin's
allow-list: a tool profile does not remove it. The plugin is linked against only the
capabilities the spec grants (PL-U4); an ungranted one it calls is refused with
`capability <x> not granted to this agent`, which the model reads, and a grant the plugin
does not request fails the build as a spec error. A call would ask for approval only if
the spec granted a side-effecting capability; none of v1's is (`file` is read-only, and
`llm` and `billing` cost money but count against the budget). Under
`--tool-loading dynamic` each plugin is one `load_tools` group named after it. What a
plugin spends through `llm::complete` runs on the calling agent's model and is recorded
as its own usage line on the turn, naming the plugin and the model that served it. A
plugin that does not load fails the agent's build. The desktop runs spec agents as
`chatty-tui` workers, which load their plugins the same way; it no longer adds its modules
to the MCP server list — `/mcp/{module}` is for MCP clients outside chatty.

A spec opts in by listing the plugin; nothing else does. Installing a plugin from the
Hive marketplace puts it in the module directory and nothing more: it is not an agent
and joins no agent until a spec names it.

## Settings → Plugins

The desktop's **Plugins** page lists every module in the module directory: version,
directory, whether it came from Hive or was copied in by hand, its trust level
(PL-H5a), its tools, whether it runs locally and is served over MCP, a load failure if
any, and **which agent specs use it with the capabilities each one grants**. The spec is
where a plugin is granted anything; the page is read-only.

## Serving a plugin over MCP

The protocol gateway ([agents-and-specs.md](agents-and-specs.md#protocol-gateway))
serves a plugin's tools to MCP clients outside chatty at `/mcp/{module}` (and the
HTTP+SSE transport at `/mcp/{module}/sse`), only when its `[protocols] mcp` is set —
otherwise 404. There is no OpenAI route and no A2A route for a plugin: it has tools,
not a loop (PL-U3, PL-U5).
