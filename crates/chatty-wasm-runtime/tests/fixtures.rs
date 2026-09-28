//! Smoke test for the WASM test fixtures (AGE-596): every good fixture loads
//! and answers `metadata`. The behaviour suites live elsewhere; this only
//! proves the assets exist and instantiate.
//!
//! Needs `scripts/build-wasm-fixtures.sh` to have run once per checkout.

use std::sync::Arc;

use chatty_wasm_runtime::test_support::{FakeLlm, fixture_path};
use chatty_wasm_runtime::{ModuleManifest, ResourceLimits, WasmModule};

/// Every fixture built on chatty-module-sdk, plus the two real plugins.
/// `wit-0.1`, `wit-0.2` and `core-module` are built to fail loading, so they
/// are not here.
const GOOD_FIXTURES: &[&str] = &[
    "echo",
    "benford",
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
    "padded",
    "billing",
    "sleep",
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
        let metadata = module
            .metadata()
            .unwrap_or_else(|e| panic!("fixture `{name}` metadata failed: {e:#}"));
        assert_eq!(metadata.name, *name, "fixture `{name}` metadata name");
    }
}
