//! S7 §1: cold load time per module, and load time for 20 copies of one
//! module (AGE-603, plugin evaluation plan §3 S7).
//!
//! `WasmModule::from_file` is synchronous (parses + Cranelift-compiles the
//! component; no guest code runs), so this needs no async runtime.
//!
//! Run: `CARGO_TARGET_DIR=<dir> cargo bench -p chatty-protocol-gateway --bench load_time`
//! Prerequisite: `bash scripts/build-wasm-fixtures.sh` (once per checkout).

#[path = "common.rs"]
mod common;

use chatty_wasm_runtime::LlmProvider;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::sync::Arc;

fn cold_load(c: &mut Criterion) {
    let engine = common::engine();
    let llm: Arc<dyn LlmProvider> = common::fake_llm("unused");

    let mut group = c.benchmark_group("cold_load");
    for fixture in ["echo-agent", "benford-agent", "padded"] {
        group.bench_function(fixture, |b| {
            b.iter(|| black_box(common::load_module(&engine, fixture, llm.clone())));
        });
    }
    group.finish();
}

/// Loading 20 copies of the same module back to back (same engine, fresh
/// `WasmModule`/store each time) — the module registry's own pattern when a
/// directory holds many instances of one plugin.
fn load_twenty_copies(c: &mut Criterion) {
    let engine = common::engine();
    let llm: Arc<dyn LlmProvider> = common::fake_llm("unused");

    let mut group = c.benchmark_group("load_20_copies");
    group.sample_size(10);
    for fixture in ["echo-agent", "benford-agent"] {
        group.bench_function(fixture, |b| {
            b.iter(|| {
                let mods: Vec<_> = (0..20)
                    .map(|_| common::load_module(&engine, fixture, llm.clone()))
                    .collect();
                black_box(mods);
            });
        });
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20);
    targets = cold_load, load_twenty_copies
}
criterion_main!(benches);
