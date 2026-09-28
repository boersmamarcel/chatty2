//! S7 §4: 1 call/second to `benford` for 60 minutes, sampling RSS, fd
//! count, and fuel-exhaustion errors (AGE-603, plugin evaluation plan §3
//! S7). Pre-PL-H1 (before AGE-604/#941), F2 predicted fuel-exhaustion errors
//! would start once *cumulative* fuel ran out, since a `WasmModule`'s fuel
//! was set once at instantiate and never refilled; the pre-H1 run in
//! `docs/research/plugin-perf-2026-09-27.md` did not actually observe one in
//! 3,601 calls (its per-call fuel cost was too low to exhaust the 10⁸
//! lifetime budget in an hour at 1 call/s). Post-H1, fuel resets before
//! every call, so this column should show none regardless.
//!
//! A plain binary (`harness = false`): this samples wall-clock and OS state
//! over real time, which criterion's statistical model isn't for.
//!
//! Run (default 60 min): in the background, since it blocks for the full
//! duration:
//!   `CARGO_TARGET_DIR=<dir> nohup cargo bench -p chatty-protocol-gateway --bench soak > soak.log 2>&1 &`
//! Override the duration for a smoke test: `SOAK_SECONDS=30 cargo bench ... --bench soak`
//! (`cargo bench` passes env vars through normally; no special flag needed).

#[path = "common.rs"]
mod common;

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chatty_wasm_runtime::{LlmProvider, ToolCallRequest};

/// Linux-only: current process RSS in KiB, from `/proc/self/status`.
fn rss_kib() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| {
        l.strip_prefix("VmRSS:")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
    })
}

/// Linux-only: open file descriptor count, from `/proc/self/fd`.
fn fd_count() -> Option<usize> {
    fs::read_dir("/proc/self/fd").ok().map(|d| d.count())
}

#[tokio::main]
async fn main() {
    let duration = std::env::var("SOAK_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(60 * 60));

    let engine = common::engine();
    // Each call is one `compute_benford_distribution` tool call, the work
    // an agent's model asks the plugin for; the tool makes no host call.
    let llm: Arc<dyn LlmProvider> = common::fake_llm_many("unused", 1);
    let mut module = common::load_module(&engine, "benford", llm);

    println!("# S7 soak: 1 call/s to benford for {:?}", duration);
    println!("elapsed_s,call,ok,error,rss_kib,fd_count");

    let start = Instant::now();
    let mut call_no: u64 = 0;
    let mut first_fuel_error_at: Option<u64> = None;
    let mut errors = 0u64;
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    while start.elapsed() < duration {
        interval.tick().await;
        call_no += 1;
        let result = module
            .invoke_tool(ToolCallRequest {
                name: "compute_benford_distribution".to_string(),
                arguments_json: r#"{"numbers":[12,34,56,78,910,1112,1314,1516,1718,1920]}"#
                    .to_string(),
                call_id: format!("soak-{call_no}"),
                caller: None,
            })
            .await;

        let (ok, error) = match &result {
            Ok(_) => (1, String::new()),
            Err(e) => {
                errors += 1;
                if first_fuel_error_at.is_none() && format!("{e:#}").to_lowercase().contains("fuel")
                {
                    first_fuel_error_at = Some(call_no);
                }
                (0, format!("{e:#}").replace(',', ";"))
            }
        };

        println!(
            "{:.1},{},{},{},{},{}",
            start.elapsed().as_secs_f64(),
            call_no,
            ok,
            error,
            rss_kib().unwrap_or(0),
            fd_count().unwrap_or(0),
        );
    }

    println!(
        "# done: {} calls, {} errors, first fuel-exhaustion error at call {:?}",
        call_no, errors, first_fuel_error_at
    );
}
