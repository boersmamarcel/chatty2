# chatty-module-sdk

SDK for authoring chatty WASM agent modules. **Compile target:
`wasm32-wasip2`.**

This crate is **intentionally outside the main workspace** (see the
`[workspace]` table at the bottom of `Cargo.toml`). Module authors depend
on it as a `path = "../../crates/chatty-module-sdk"` from their own
single-crate project rooted at the module directory.

## What it provides

- **WIT types** re-exported as a flat module namespace
  (`ChatRequest`, `ChatResponse`, `Message`, `Role`, …)
- **Host imports** — `llm::complete`, `config::get`, `log::info`/`warn`/`error`
- **[`ModuleExports`] trait** — the trait module authors implement
- **[`export_module!`] macro** — wires a `ModuleExports` impl into the
  WIT guest exports the runtime expects

## Quick start

See the rustdoc on [`ModuleExports`] in [`src/lib.rs`](src/lib.rs) for a
working example, and the reference modules under
[`../../modules/`](../../modules/) (`echo-agent`, `benford-agent`).

## Build

```bash
rustup target add wasm32-wasip2
cd modules/your-module
cargo build --target wasm32-wasip2 --release
```

The output `.wasm` lives under
`target/wasm32-wasip2/release/your_module.wasm`. Copy or symlink it next
to your `module.toml` so the registry can discover it.

[`ModuleExports`]: src/lib.rs
[`export_module!`]: src/lib.rs
