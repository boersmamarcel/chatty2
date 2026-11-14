# Plan: AGE-1110 / AGE-1394 item c — gpui stops wiring credit guard + usage collector

## Acceptance
- `rg -w 'CreditGuard|UsageCollector|UsageCollectorConfig' crates/chatty-gpui` finds nothing
- `cargo check -p chatty-gpui` passes where it can run
- Only module_settings_controller.rs (and .chatty state files) change

## Steps
1. [x] Replace credit guard/collector/meter block with `ModuleMeter::new(paid_modules)`; drop unused hive imports and Hive client/session handling (commit: see git log)
