//! S7 §3: throughput and p50/p95 latency at 1/8/32 concurrent clients,
//! against one module and against two modules (AGE-603, plugin evaluation
//! plan §3 S7). Not a criterion microbench — it reports percentiles over a
//! fixed request count per client, which criterion's own model doesn't
//! give directly — so this is a plain binary (`harness = false`).
//!
//! Run: `CARGO_TARGET_DIR=<dir> cargo bench -p chatty-protocol-gateway --bench concurrency`
//! (release profile; `cargo bench` builds in `bench` profile, which is
//! release-equivalent).

#[path = "common.rs"]
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

/// Requests each client issues per (client-count, module-set) cell. Kept
/// small enough that all cells together finish in well under a minute.
const REQUESTS_PER_CLIENT: usize = 200;

struct Cell {
    label: &'static str,
    clients: usize,
    latencies_ms: Vec<f64>,
    wall: Duration,
}

impl Cell {
    fn report(&self) {
        let mut sorted = self.latencies_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p = |q: f64| -> f64 {
            let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
            sorted[idx]
        };
        let total = sorted.len();
        let throughput = total as f64 / self.wall.as_secs_f64();
        println!(
            "{:<28} clients={:<3} n={:<5} throughput={:>8.1} req/s  p50={:>7.2} ms  p95={:>7.2} ms  p99={:>7.2} ms  max={:>7.2} ms",
            self.label,
            self.clients,
            total,
            throughput,
            p(0.50),
            p(0.95),
            p(0.99),
            sorted.last().copied().unwrap_or(0.0),
        );
    }
}

/// One MCP `tools/call` against `module` through `base` (the one route a
/// plugin is served on, PL-U3); neither tool touches the fake LLM.
async fn one_call(http: &reqwest::Client, base: &str, module: &str) {
    let url = format!("{base}/mcp/{module}");
    let params = match module {
        "benford" => json!({
            "name": "compute_benford_distribution",
            "arguments": {"numbers": [123.4, 187.0, 1450.0, 212.5, 2999.0]},
        }),
        _ => json!({"name": "echo", "arguments": {"input": "hello"}}),
    };
    let resp = http
        .post(&url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params}))
        .send()
        .await
        .unwrap_or_else(|e| panic!("request to {module}: {e}"));
    assert!(resp.status().is_success(), "{module}: {}", resp.status());
}

async fn run_cell(
    label: &'static str,
    base: Arc<String>,
    clients: usize,
    modules: &'static [&'static str],
) -> Cell {
    let http = Arc::new(reqwest::Client::new());
    let start = Instant::now();
    let mut handles = Vec::with_capacity(clients);
    for c in 0..clients {
        let http = http.clone();
        let base = base.clone();
        let module = modules[c % modules.len()];
        handles.push(tokio::spawn(async move {
            let mut lat = Vec::with_capacity(REQUESTS_PER_CLIENT);
            for _ in 0..REQUESTS_PER_CLIENT {
                let t0 = Instant::now();
                one_call(&http, &base, module).await;
                lat.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            lat
        }));
    }
    let mut latencies_ms = Vec::new();
    for h in handles {
        latencies_ms.extend(h.await.expect("client task"));
    }
    let wall = start.elapsed();
    Cell {
        label,
        clients,
        latencies_ms,
        wall,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 28)]
async fn main() {
    let (base, _llm) = common::start_gateway(&["echo", "benford"]).await;
    let base = Arc::new(base);

    println!(
        "# S7 concurrency: {REQUESTS_PER_CLIENT} requests/client, MCP tools/call (no LLM call)"
    );
    for &clients in &[1usize, 8, 32] {
        run_cell(
            "one module (echo)",
            base.clone(),
            clients,
            &["echo"],
        )
        .await
        .report();
    }
    for &clients in &[1usize, 8, 32] {
        run_cell(
            "two modules (echo+benford)",
            base.clone(),
            clients,
            &["echo", "benford"],
        )
        .await
        .report();
    }
}
