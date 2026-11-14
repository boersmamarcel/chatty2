# Progress

## 2026-11-14
- Steps 1-4 done in one commit on auto/age-1110a-gateway-stops-reporting-usage.
- fmt, clippy -D warnings: clean.
- cargo test: lib 140 pass / 1 fail (participant::runner::...finds_a_socket_there, fails on the base too);
  wasm-fixture tests fail because target/wasm-fixtures is missing (no wasm32-wasip2 target here).
