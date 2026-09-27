//! Smoke test for the WASM test fixtures (AGE-596): every good fixture loads
//! and answers `get-agent-card`. The behaviour suites live elsewhere; this only
//! proves the assets exist and instantiate.
//!
//! Needs `scripts/build-wasm-fixtures.sh` to have run once per checkout.

use std::sync::Arc;

use chatty_wasm_runtime::test_support::{FakeLlm, fixture_path};
use chatty_wasm_runtime::{ModuleManifest, ResourceLimits, WasmModule};

/// Every fixture built on chatty-module-sdk, plus the two real modules.
/// `wit-0.1` and `core-module` are built to fail loading, so they are not here.
const GOOD_FIXTURES: &[&str] = &[
    "echo-agent",
    "benford-agent",
    "spin",
    "slow-host",
    "fuel-meter",
    "alloc",
    "panic",
    "trap",
    "huge-output",
    "stateful",
    "config-reader",
    "file-reader",
    "log-flood",
    "tool-args",
    "threads",
];

#[test]
fn fixtures_load() {
    let limits = ResourceLimits::default();
    let engine = WasmModule::build_engine(&limits).expect("engine");
    for name in GOOD_FIXTURES {
        let mut module = WasmModule::from_file(
            &engine,
            &fixture_path(name),
            ModuleManifest::new(*name),
            Arc::new(FakeLlm::default()),
            limits.clone(),
        )
        .unwrap_or_else(|e| panic!("fixture `{name}` failed to load: {e:#}"));
        let card = module
            .agent_card()
            .unwrap_or_else(|e| panic!("fixture `{name}` get-agent-card failed: {e:#}"));
        assert_eq!(card.name, *name, "fixture `{name}` card name");
    }
}
