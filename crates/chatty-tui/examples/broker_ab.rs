//! AGE-302 — ADR-0011's two kill criteria, measured.
//!
//! One fixed delegated task, run N times through `sub_agent` and N times
//! through `invoke_agent` over the broker, on the same host, against the same
//! model, with no VM. Reports:
//!
//! 1. the parent's rendered progress events for each path, so they can be
//!    diffed by count, order, tool names and interleaved text;
//! 2. end-to-end wall clock P50 / P95 for both.
//!
//! The criterion fires if a progress event class is lost, or if the broker
//! path is slower than `sub_agent` by more than ~20 ms at P50.
//!
//! # Running it
//!
//! Through `scripts/broker-ab-measurement.sh`, which builds release, writes an
//! isolated `XDG_CONFIG_HOME` so the run cannot touch a real chatty setup,
//! and puts the release `chatty-tui` on `PATH` — both tools spawn it by name.
//!
//! # What this does not measure
//!
//! Whether the *mapping* preserves every event class. That is answered
//! exhaustively and deterministically by
//! `crates/chatty-tui/src/participant/equivalence.rs`, which diffs the two
//! paths' traces across every scripted scenario in CI. A live model calls
//! whichever tools it feels like, which makes it a weak instrument for a
//! completeness question and a good one for latency.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chatty_core::services::install_progress_channel;
use chatty_core::tools::invoke_agent_tool::{
    InvokeAgentArgs, InvokeAgentProgress, InvokeAgentProgressSlot, InvokeAgentTool,
};
use chatty_core::tools::sub_agent_tool::{SubAgentArgs, SubAgentTool};
use chatty_core::tools::{LOCAL_AGENT_NAME, worker_executable};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::LocalRunner;
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use parking_lot::Mutex;
use rig_agent::tool::{Tool, ToolContext};
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

struct Config {
    runs: usize,
    model: String,
    task: String,
    socket: std::path::PathBuf,
}

impl Config {
    fn from_env() -> Self {
        Self {
            runs: env_or("RUNS", "10").parse().expect("RUNS must be a number"),
            model: env_or("MODEL", "ollama-qwen2.5-0.5b"),
            task: env_or(
                "TASK",
                "Run the shell command `echo hello` and then reply with exactly the word done.",
            ),
            socket: std::path::PathBuf::from(env_or("SOCKET", "/tmp/age302-participants.sock")),
        }
    }
}

// ---------------------------------------------------------------------------
// One run's result
// ---------------------------------------------------------------------------

struct Run {
    elapsed: Duration,
    ok: bool,
    progress: Vec<String>,
    response: String,
}

/// One line per progress event, in the shape the parent renders.
fn describe(event: &InvokeAgentProgress) -> String {
    match event {
        InvokeAgentProgress::Started { agent_name, .. } => format!("Started({agent_name})"),
        InvokeAgentProgress::Text(text) => format!("Text({text:?})"),
        InvokeAgentProgress::Finished { success, .. } => format!("Finished(success={success})"),
    }
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

/// Nearest-rank percentile. With ten samples P95 is the maximum; that is a
/// property of the sample size the issue asked for, and the report says so
/// rather than hiding it behind interpolation.
fn percentile(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return f64::NAN;
    }
    let rank = ((p / 100.0) * sorted_ms.len() as f64).ceil().max(1.0) as usize;
    sorted_ms[rank.min(sorted_ms.len()) - 1]
}

struct Stats {
    n: usize,
    failures: usize,
    p50: f64,
    p95: f64,
    min: f64,
    max: f64,
    mean: f64,
}

fn stats(runs: &[Run]) -> Stats {
    let mut ms: Vec<f64> = runs
        .iter()
        .filter(|r| r.ok)
        .map(|r| r.elapsed.as_secs_f64() * 1000.0)
        .collect();
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = if ms.is_empty() {
        f64::NAN
    } else {
        ms.iter().sum::<f64>() / ms.len() as f64
    };
    Stats {
        n: ms.len(),
        failures: runs.iter().filter(|r| !r.ok).count(),
        p50: percentile(&ms, 50.0),
        p95: percentile(&ms, 95.0),
        min: ms.first().copied().unwrap_or(f64::NAN),
        max: ms.last().copied().unwrap_or(f64::NAN),
        mean,
    }
}

// ---------------------------------------------------------------------------
// The two arms
// ---------------------------------------------------------------------------

struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> Result<CompletionResponse, String> {
        Err("the gateway's module path is unused in this measurement".into())
    }
}

/// Start a broker: a gateway on an ephemeral port, a participant socket, and
/// the `local-agent` runner that spawns a child per task.
///
/// No workspace factory: `sub_agent` is configured without one too, so
/// neither arm pays for a `git worktree`. The criterion is about the hop.
async fn start_broker(config: &Config) -> u16 {
    let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
    let modules = Arc::new(RwLock::new(
        ModuleRegistry::new(provider, ResourceLimits::default()).unwrap(),
    ));

    let _ = std::fs::remove_file(&config.socket);
    let gateway = ProtocolGateway::new(modules, 0);
    let participants = gateway.participants();

    let listener = chatty_protocol_gateway::participant::bind(&config.socket)
        .expect("the participant socket binds");
    tokio::spawn(chatty_protocol_gateway::participant::serve(
        listener,
        participants.clone(),
    ));

    let runner = LocalRunner::new(worker_executable(), &config.socket, participants)
        .with_agent_name(LOCAL_AGENT_NAME)
        .with_args(["--model", &config.model, "--auto-approve"]);

    let gateway = gateway.with_local_runner(Arc::new(runner));

    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port");
    let port = tcp.local_addr().unwrap().port();
    let router = gateway.build_router();
    tokio::spawn(async move {
        axum::serve(tcp, router).await.ok();
    });
    port
}

async fn run_sub_agent(config: &Config) -> Run {
    // Both tools report through an `InvokeAgentProgressSlot`; `sub_agent`
    // takes one rather than owning it, so the harness listens on the same
    // channel the parent UI would.
    let slot: InvokeAgentProgressSlot = Arc::new(Mutex::new(None));
    let tool = SubAgentTool::new(
        config.model.clone(),
        true,
        Vec::new(),
        slot.clone(),
        // No workspace: see `start_broker`.
        None,
    );
    let mut progress_rx = install_progress_channel(&slot);

    let started = Instant::now();
    let result = tool
        .call(
            &mut ToolContext::new(),
            SubAgentArgs {
                task: config.task.clone(),
                model: None,
            },
        )
        .await;
    let elapsed = started.elapsed();

    let mut progress = Vec::new();
    while let Ok(event) = progress_rx.try_recv() {
        progress.push(describe(&event));
    }

    match result {
        Ok(output) => Run {
            elapsed,
            ok: output.success,
            progress,
            response: output.response,
        },
        Err(e) => Run {
            elapsed,
            ok: false,
            progress,
            response: format!("{e}"),
        },
    }
}

async fn run_invoke_agent(config: &Config, port: u16) -> Run {
    let tool =
        InvokeAgentTool::new(Vec::new(), Vec::new(), Some(port)).with_local_agent(LOCAL_AGENT_NAME);
    let mut progress_rx = install_progress_channel(&tool.progress_slot());

    let started = Instant::now();
    let result = tool
        .call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: LOCAL_AGENT_NAME.to_string(),
                prompt: config.task.clone(),
            },
        )
        .await;
    let elapsed = started.elapsed();

    let mut progress = Vec::new();
    while let Ok(event) = progress_rx.try_recv() {
        progress.push(describe(&event));
    }

    match result {
        Ok(output) => Run {
            elapsed,
            ok: output.success,
            progress,
            response: output.response,
        },
        Err(e) => Run {
            elapsed,
            ok: false,
            progress,
            response: format!("{e}"),
        },
    }
}

// ---------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------

fn report_arm(name: &str, runs: &[Run]) -> Stats {
    let s = stats(runs);
    println!("### {name}");
    println!();
    println!(
        "| runs | ok | failed | P50 ms | P95 ms | min ms | max ms | mean ms |\n\
         |---|---|---|---|---|---|---|---|"
    );
    println!(
        "| {} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
        runs.len(),
        s.n,
        s.failures,
        s.p50,
        s.p95,
        s.min,
        s.max,
        s.mean
    );
    println!();
    s
}

fn fmt_deltas(deltas: &[f64]) -> String {
    deltas
        .iter()
        .map(|d| format!("{d:+.1}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn report_progress(name: &str, runs: &[Run]) {
    let Some(first) = runs.iter().find(|r| r.ok).or_else(|| runs.first()) else {
        return;
    };
    println!("**{name} — the parent's rendered progress (first run):**");
    println!();
    println!("```");
    for line in &first.progress {
        println!("{line}");
    }
    println!("```");
    println!();
    println!("Event count per run: {:?}", counts(runs));
    println!();
    println!("Distinct progress lines across all runs:");
    println!();
    println!("```");
    for line in classes(runs) {
        println!("{line}");
    }
    println!("```");
    println!();
    println!("Response: `{}`", first.response.replace('\n', " "));
    println!();
}

fn counts(runs: &[Run]) -> Vec<usize> {
    runs.iter().map(|r| r.progress.len()).collect()
}

/// Every distinct progress line across all runs, so the two arms can be
/// compared on which *classes* of event reached the parent rather than on one
/// sampled run. A live model calls different tools on different runs; a lost
/// event class would show up as a line one arm never produces.
fn classes(runs: &[Run]) -> Vec<String> {
    let mut seen: Vec<String> = runs
        .iter()
        .flat_map(|r| r.progress.iter().cloned())
        .collect();
    seen.sort();
    seen.dedup();
    seen
}

/// Paired per-run differences, in the order the runs were interleaved.
///
/// The right statistic for this question: the arms alternate, so a drift in
/// machine load or model-server state lands on both members of a pair and
/// cancels. Comparing two independent P50s over ten samples would not resolve
/// 20 ms against a model turn that takes hundreds.
fn paired_deltas(sub: &[Run], broker: &[Run]) -> Vec<f64> {
    sub.iter()
        .zip(broker)
        .filter(|(s, b)| s.ok && b.ok)
        .map(|(s, b)| (b.elapsed.as_secs_f64() - s.elapsed.as_secs_f64()) * 1000.0)
        .collect()
}

#[tokio::main]
async fn main() {
    let config = Config::from_env();

    eprintln!(
        "AGE-302: {} runs per arm, model {}, task {:?}",
        config.runs, config.model, config.task
    );

    let port = start_broker(&config).await;
    eprintln!(
        "broker on 127.0.0.1:{port}, socket {}",
        config.socket.display()
    );

    // Warm-up, excluded from the statistics: the first run of either arm pays
    // for a cold page cache on the binary and a cold model in the server,
    // which is a property of the machine and not of the hop under test.
    eprintln!("warm-up …");
    let _ = run_sub_agent(&config).await;
    let _ = run_invoke_agent(&config, port).await;

    let mut sub_runs = Vec::new();
    let mut broker_runs = Vec::new();
    // Interleaved so a drift in machine load over the session cannot land on
    // one arm — the thing being measured is a ~20 ms difference.
    for i in 0..config.runs {
        eprintln!("run {}/{}", i + 1, config.runs);
        sub_runs.push(run_sub_agent(&config).await);
        broker_runs.push(run_invoke_agent(&config, port).await);
    }

    println!("## Results");
    println!();
    let sub = report_arm("`sub_agent` (baseline)", &sub_runs);
    let broker = report_arm("`invoke_agent` over the broker", &broker_runs);

    println!("### Kill criterion 2 — the hop");
    println!();

    let unpaired = broker.p50 - sub.p50;
    let mut deltas = paired_deltas(&sub_runs, &broker_runs);
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let paired_p50 = percentile(&deltas, 50.0);
    let paired_p95 = percentile(&deltas, 95.0);
    let slower = deltas.iter().filter(|d| **d > 0.0).count();

    println!("| statistic | value |");
    println!("|---|---|");
    println!("| P50(broker) - P50(sub_agent) | {unpaired:+.1} ms |");
    println!("| **median paired delta** | **{paired_p50:+.1} ms** |");
    println!("| P95 paired delta | {paired_p95:+.1} ms |");
    println!(
        "| runs where the broker was slower | {slower} of {} |",
        deltas.len()
    );
    println!("| all paired deltas (ms) | {} |", fmt_deltas(&deltas));
    println!();
    println!(
        "The paired delta is the statistic that decides this: the arms are \
         interleaved, so machine drift lands on both members of a pair and \
         cancels, while the difference of two independent P50s over ten \
         samples is dominated by the model turn's own variance."
    );
    println!();
    println!(
        "> {}",
        if paired_p50 > 20.0 {
            "**FIRES.** The hop costs more than process spawn; ADR-0011 says the \
             local runner then moves in-process once AGE-193 lands, and the \
             broker is reserved for boundary crossings."
        } else {
            "**Does not fire.** The hop is within process-spawn cost."
        }
    );
    println!();

    println!("### Kill criterion 1 — the progress diff");
    println!();
    report_progress("`sub_agent`", &sub_runs);
    report_progress("broker", &broker_runs);

    let _ = std::fs::remove_file(&config.socket);
}
