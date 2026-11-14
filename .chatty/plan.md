# Plan: AGE-1110 item b - core meter without collector

## Acceptance criteria
- `grep -rwE 'CallUsage|UsageCollector|UsageCollectorConfig|CreditGuard|call_usage' crates/chatty-core` finds nothing.
- No test mocks /api/usage/report or /pricing.
- `paid_plugin_in_spec_is_refused_on_desktop` passes; a free plugin still runs.
- Only plugin_tool.rs, extensions_store.rs (plus the removed `hive` arg at the one caller, chatty-tui engine) change.

## Steps
1. [x] Meter: borrowed_meter -> ModuleMeter::new(paid) only; drop `hive` arg and caller arg; remove `.record` and `call_usage`; replace retired-route tests; update comments. (single commit)
