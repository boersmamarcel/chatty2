# chatty-module-registry

Module discovery, loading, and lifecycle management for chatty WASM
agent modules.

This crate sits on top of [`chatty-wasm-runtime`](../chatty-wasm-runtime/)
and adds:

- **Discovery** — scan a directory for `.wasm` files paired with
  `module.toml` manifests
- **Manifest parsing** — typed `ModuleManifest` with capabilities,
  protocols, and resource limits
- **Lifecycle** — load, unload, reload, enumerate currently loaded modules

There is no filesystem watcher: an agent that runs a module as a plugin
loads its own instance when it is built (PL-U2), so a changed module is
picked up by the next agent, and by `reload` or a new scan here.

## Public surface

- [`ModuleRegistry`] — owns all loaded modules, exposes
  `scan_directory`, `load`, `unload`, `reload`, `get`, `manifest`,
  `module_names`, `len` and `is_empty`
- [`ModuleManifest`] + [`ModuleCapabilities`], [`ModuleProtocols`],
  [`ModuleResourceLimits`]

See the rustdoc on [`ModuleRegistry`] for the canonical usage example.

## Where modules live by default

Platform-native app-data directories, not a single fixed path:
`~/Library/Application Support/chatty/modules/` on macOS,
`~/.local/share/chatty/modules/` (or `$XDG_DATA_HOME/chatty/modules/`) on
Linux, `%APPDATA%\chatty\modules\` on Windows — overridable in module
settings. `.chatty/modules` (relative to the working directory) is only a
last-resort fallback when the platform data directory can't be determined.
Each subdirectory is one module: a `.wasm` plus a `module.toml`.

See [`docs/a2a-and-wasm-modules.md`](../../docs/a2a-and-wasm-modules.md)
for the end-to-end module flow.

## Build / test

```bash
cargo test -p chatty-module-registry
```

[`ModuleRegistry`]: src/registry.rs
[`ModuleManifest`]: src/manifest.rs
[`ModuleCapabilities`]: src/manifest.rs
[`ModuleProtocols`]: src/manifest.rs
[`ModuleResourceLimits`]: src/manifest.rs
