//! Trip-wire for the "must trap" fixtures (AGE-600, evaluation plan §4.6).
//!
//! PL-E2's full sandbox suite (`tests/sandbox.rs`, AGE-597) covers every S1
//! row with its correct pass criterion, including several that are currently
//! `#[ignore]`d as known defects (PL-H1/AGE-604, PL-H3/AGE-606). This test is
//! narrower and deliberately does not depend on that suite (open PR #934 at
//! the time of writing): it only asserts the handful of "must trap" fixtures
//! that already succeed today, so a sandbox regression that lets a
//! known-hostile module load or run to completion fails CI immediately
//! instead of rotting unnoticed the way the rest of the plugin path did.
//!
//! Deliberately excluded: `panic` and `alloc` over its cap. Per the
//! evaluation plan's S1 results (row 1.6/1.7), hitting the memory limiter or
//! a Rust-level guest panic today re-enters the calling Tokio runtime via a
//! WASI-stdout-flush bug (PL-H1/AGE-604) instead of surfacing a clean `Err`.
//! Exercising them here would make this trip-wire flaky on the very defect
//! it doesn't own; that behaviour is `sandbox.rs`'s job.
//!
//! Needs `scripts/build-wasm-fixtures.sh` to have run once per checkout.

use std::sync::Arc;

use chatty_wasm_runtime::test_support::{FakeLlm, fixture_path};
use chatty_wasm_runtime::{ChatRequest, Message, ModuleManifest, ResourceLimits, Role, WasmModule};

fn hello() -> ChatRequest {
    ChatRequest {
        messages: vec![Message {
            role: Role::User,
            content: "hello".to_string(),
        }],
        conversation_id: "must-trap".to_string(),
    }
}

/// A component built against an old WIT world version must be refused at
/// load, not silently accepted with mismatched exports.
#[test]
fn wit_version_mismatch_fails_to_load() {
    let limits = ResourceLimits::default();
    let engine = WasmModule::build_engine(&limits).expect("engine");
    let result = WasmModule::from_file(
        &engine,
        &fixture_path("wit-0.1"),
        ModuleManifest::new("wit-0.1"),
        Arc::new(FakeLlm::default()),
        limits,
    );
    let err = match result {
        Ok(_) => {
            panic!("a component built against chatty:module@0.1.0 must not load against 0.2.0")
        }
        Err(e) => e,
    };
    let message = format!("{err:#}");
    assert!(
        !message.is_empty(),
        "load failure must name a cause, not just fail"
    );
}

/// A plain core WASM module (not a component) must be rejected at load, not
/// treated as a valid module with no exports.
#[test]
fn core_module_fails_to_load() {
    let limits = ResourceLimits::default();
    let engine = WasmModule::build_engine(&limits).expect("engine");
    if WasmModule::from_file(
        &engine,
        &fixture_path("core-module"),
        ModuleManifest::new("core-module"),
        Arc::new(FakeLlm::default()),
        limits,
    )
    .is_ok()
    {
        panic!("a core (non-component) module must be refused, not loaded as an agent");
    }
}

/// A module that never returns must trap on fuel exhaustion under the
/// default limits, not run forever or silently return a bogus response. If
/// this starts passing (fuel refill lands, PL-H1/AGE-604), tighten the
/// assertion instead of deleting it — it must still trap eventually.
#[tokio::test]
async fn infinite_loop_traps_on_fuel_exhaustion() {
    let limits = ResourceLimits::default();
    let engine = WasmModule::build_engine(&limits).expect("engine");
    let mut module = WasmModule::from_file(
        &engine,
        &fixture_path("spin"),
        ModuleManifest::new("spin"),
        Arc::new(FakeLlm::default()),
        limits,
    )
    .expect("spin loads: only its `chat` call misbehaves");

    let start = std::time::Instant::now();
    module
        .chat(hello())
        .await
        .expect_err("an infinite loop must trap on fuel exhaustion, not return a response");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "fuel exhaustion must trap promptly, not hang until the wall-clock timeout"
    );
}

/// A guest that executes `unreachable` must surface as a clean `Err`, never
/// as a value the caller could mistake for a real response.
#[tokio::test]
async fn unreachable_instruction_traps() {
    let limits = ResourceLimits::default();
    let engine = WasmModule::build_engine(&limits).expect("engine");
    let mut module = WasmModule::from_file(
        &engine,
        &fixture_path("trap"),
        ModuleManifest::new("trap"),
        Arc::new(FakeLlm::default()),
        limits,
    )
    .expect("trap loads: only its `chat` call misbehaves");

    module
        .chat(hello())
        .await
        .expect_err("`unreachable` must trap, not return a value");
}
