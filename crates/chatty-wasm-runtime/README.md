# chatty-wasm-runtime

Wasmtime embedding and host-side WIT interface for chatty WASM plugins.

This crate wraps `wasmtime` 30 with the component model enabled, loads
WASM components compiled to `wasm32-wasip2` against `chatty:plugin@0.4.0`,
applies resource limits, and provides the host side of the plugin imports
(`llm`, `config`, `logging`, `file`, `billing`, one interface per
capability).

## Public surface

- [`WasmModule`] — a loaded plugin: `metadata()`, `list_tools()`, `invoke_tool()`
- [`ResourceLimits`] — fuel / memory / wall-clock / output-size caps applied per call (PL-D3/PL-D3b: 10¹² fuel, 256 MiB, 60 s including host time, 1 MiB output; a manifest may only lower these)
- [`Engine`] — re-exported `wasmtime::Engine` so callers can share one engine
- Host traits: [`LlmProvider`], [`BillingProvider`]
- Errors: `CallError` (a limit or a trap), `ToolFailure` (the guest's own `tool-error`)
- WIT types: `PluginMetadata`, `Capability`, `ConfigKey`, `ToolCallRequest`, `ToolResult`, `ToolError`, `ToolErrorKind`, `ToolDefinition`, `Message`, `Role`, `ToolCall`, `TokenUsage`, `CompletionResponse`, `SessionInfo`

Most callers go through [`chatty-module-registry`](../chatty-module-registry/)
instead of this crate directly.

## WIT versioning

A single `bindgen!` invocation lives in `lib.rs`: `bindings`, generated from
the repo-root `wit/` directory, which is `chatty:plugin@0.4.0` (`WIT_PACKAGE`).

Only that package is loaded. A component that does not export
`chatty:plugin/plugin@0.4.0` is refused at load with
`module targets chatty:module@0.2.0; this chatty supports chatty:plugin@0.4.0
— rebuild it with the current SDK` (naming whatever world it does target):
there is no adapter for an older world (PL-D1). The build fails if the WIT
file declares another package until `WIT_PACKAGE`/`PLUGIN_EXPORT` (and the
SDK's `WIT_PACKAGE`) are updated with it.

See [`docs/wit-reference.md`](../../docs/wit-reference.md) for the WIT
schema and [`docs/plugins.md`](../../docs/plugins.md)
for the broader module architecture.

## Build / test

```bash
cargo test -p chatty-wasm-runtime
```

The unit tests need no WASM; the `fixtures` and `sandbox` suites load real
fixture components, built once per checkout by
`scripts/build-wasm-fixtures.sh` (`--features test-support`).

[`WasmModule`]: src/module.rs
[`ResourceLimits`]: src/limits.rs
[`LlmProvider`]: src/host.rs
[`BillingProvider`]: src/host.rs
