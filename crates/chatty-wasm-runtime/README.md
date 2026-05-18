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

Two `bindgen!` invocations live in `lib.rs`:

- `bindings` — current `chatty:module@0.2.0` (path: `wit/`)
- `bindings_v0_1` — legacy `chatty:module@0.1.0` (path: `wit-v0_1/`)

Both are registered in the linker so older pre-0.2.0 modules continue to
load. The 0.1.0 and 0.2.0 interfaces are byte-for-byte identical except
that 0.2.0 adds the optional `billing` interface.

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
