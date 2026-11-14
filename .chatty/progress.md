## 2026-11-14
- Step 1 done: borrowed_meter is ModuleMeter::new(paid) only; usage reporting and mocked retired-route tests removed. Caller outside core: chatty-tui engine/mod.rs (dropped hive arg and hive settings load). chatty-gpui module_settings_controller.rs still uses CreditGuard/UsageCollector (outside scope, not edited).
