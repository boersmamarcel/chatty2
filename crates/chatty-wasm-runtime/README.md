# chatty-wasm-runtime

Wasmtime embedding and host-side WIT interface for chatty WASM modules.

This crate wraps `wasmtime` 30 with the component model enabled, loads
WASM components compiled to `wasm32-wasip2`, applies resource limits, and
provides the host-side implementation of the WIT interfaces (`llm`,
`config`, `logging`, optional `billing`).

## Public surface

- [`WasmModule`] — loaded, callable module instance
- [`ResourceLimits`] — fuel / memory / table caps applied per module
- [`Engine`] — re-exported `wasmtime::Engine` so callers can share one engine
- Host traits: [`LlmProvider`], [`BillingProvider`]
- WIT types: `AgentCard`, `ChatRequest`, `ChatResponse`, `Message`, `Role`, `Skill`, `TokenUsage`, `ToolCall`, `ToolDefinition`

Most callers go through [`chatty-module-registry`](../chatty-module-registry/)
instead of this crate directly.

## WIT versioning

A single `bindgen!` invocation lives in `lib.rs`: `bindings`, generated from
the repo-root `wit/` directory, which is `chatty:module@0.2.0`.

Only that package version is registered in the linker. A module targeting an
older package version fails to instantiate — the host exposes exactly one WIT
version at a time and modules are rebuilt against it, in line with the
project's no-compatibility-shim policy.

See [`docs/wit-reference.md`](../../docs/wit-reference.md) for the WIT
schema and [`docs/a2a-and-wasm-modules.md`](../../docs/a2a-and-wasm-modules.md)
for the broader module architecture.

## Build / test

```bash
cargo test -p chatty-wasm-runtime
```

Tests do not require a separately built WASM module — fixtures are
generated inline.

[`WasmModule`]: src/module.rs
[`ResourceLimits`]: src/limits.rs
[`LlmProvider`]: src/host.rs
[`BillingProvider`]: src/host.rs
