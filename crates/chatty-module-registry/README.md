# chatty-module-registry

Module discovery, loading, and lifecycle management for chatty WASM
agent modules.

This crate sits on top of [`chatty-wasm-runtime`](../chatty-wasm-runtime/)
and adds:

- **Discovery** — scan a directory for `.wasm` files paired with
  `module.toml` manifests
- **Manifest parsing** — typed `ModuleManifest` with capabilities,
  protocols, and resource limits
- **Hot-reload** — `notify`-based filesystem watcher that re-loads
  changed modules
- **Lifecycle** — start, stop, enumerate currently loaded modules

## Public surface

- [`ModuleRegistry`] — owns all loaded modules, exposes
  `scan_directory`, `start_watcher`, `list`, `get`
- [`ModuleManifest`] + [`ModuleCapabilities`], [`ModuleProtocols`],
  [`ModuleResourceLimits`]

See the rustdoc on [`ModuleRegistry`] for the canonical usage example.

## Where modules live by default

`~/.chatty/modules/` (override via `ChattyApp` settings). Each subdirectory
is one module: a `.wasm` plus a `module.toml`.

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
