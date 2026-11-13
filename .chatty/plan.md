# AGE-1110 plan: hive-client stops reporting usage and reading retired routes

## Acceptance criteria
- `grep -rE 'report_usage|get_module_pricing|acquire_session|settle_session|UsageCollector' crates/hive-client` finds nothing
- No test or fixture in crates/hive-client refers to a retired route
- `cargo test -p hive-client`, `cargo clippy -p hive-client --all-targets -- -D warnings`, `cargo fmt --all --check` pass
- `ModuleMeter::admit` refuses a paid module with `paid modules run hosted`; a free module runs
- Only files under crates/hive-client/ (plus these .chatty state files) change

## Steps
1. [x] Add `paid_module_is_refused_on_desktop` and `free_module_still_runs`; change `admit` (commit: 6a4c30c2)
2. [x] Remove usage collector, `report_usage`, usage.rs, usage_batching.rs, usage_report fixture (commit: afeda5d0)
3. [x] Remove pricing and session calls, fixtures, CreditGuard if unused, unused types (commit: 3617d89c)
4. [x] Update remaining tests and README (commit: this commit "hive-client: README says paid modules run hosted")
5. [ ] Run all check commands (commit: pending)
