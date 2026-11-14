# Plan: replace retired usage/report and /pricing mocks in plugin_tool tests

## Acceptance criteria
- No `usage/report`, `/pricing`, `paid_echo_meter`, old metering tests in plugin_tool.rs
- New tests `paid_plugin_in_spec_is_refused_on_desktop`, `free_plugin_runs_with_an_unbilled_meter`
- clippy -D warnings and fmt --check pass; production code untouched
- `cargo test -p chatty-core --lib plugin_tool -- --test-threads=1` passes (needs echo.wasm)

## Steps
1. [x] Rewrite metering section of `mod tests` (commit: "Replace retired Hive metering mocks in plugin_tool tests")
