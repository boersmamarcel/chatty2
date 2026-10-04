//! `chatty-tui --eval-team name@version --runs k` (MK-T6, AGE-851): run a
//! team's `[eval]` tasks headless and write what happened as one JSON
//! result, the score a marketplace listing shows.
//!
//! The team is the spec `name` as `--agent` would find it (installed teams
//! live in the data directory); when it was installed from the registry, the
//! installed version must be `version`. Each task runs `k` times, each time
//! in a fresh workspace holding the task's fixture files: a child
//! `chatty-tui --headless --auto-approve` runs as the team's leader (with
//! `--broker` when it delegates), its answer goes to a file outside the
//! workspace, and then the task's verifier runs in the workspace with
//! `CHATTY_EVAL_ANSWER` naming that file. Exit 0 passes. This is the
//! swarm-bench runner's shape (`scripts/swarm-bench/bench.py`, EV-3), in
//! Rust and on one team.
//!
//! The result is signed by nobody: it is what this machine measured. The
//! registry takes it as **self-reported** from the publisher and as
//! **verified** only from an operator who re-ran it. It names the model
//! every run was on and the bundle hash its tasks came from, and a task
//! counts as passed only when it passed in all `k` runs.
//!
//! The model and provider flags (`--model`, `--openai-compat-url`,
//! `--api-key`, `--ollama`, `--think`) and the budgets (`--max-agent-turns`,
//! `--max-duration`) are handed to every run.

use anyhow::{Context, Result, bail};
use chatty_core::agent_eval::EvalTask;
use chatty_core::agent_spec::{AgentSpec, load_agent_spec};
use chatty_core::team_install::{TeamRecord, parse_team_ref};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The result's `schema`; bumped only when a field changes meaning.
pub const EVAL_RESULT_SCHEMA: u32 = 1;

/// How long a verifier may run.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(120);

/// A run's hard stop past its own `--max-duration` (default 30 minutes),
/// for a child that hangs instead of finishing its last pass.
const RUN_GRACE: Duration = Duration::from_secs(300);
const DEFAULT_RUN_BUDGET: Duration = Duration::from_secs(30 * 60);

/// What `--eval-team` was asked to do.
pub struct EvalOptions {
    pub team_ref: String,
    pub runs: u32,
    pub out: Option<PathBuf>,
    /// Flags handed to every run: the model, provider and budgets.
    pub forward: Vec<String>,
    pub max_duration: Option<Duration>,
}

/// The JSON `--eval-team` writes.
#[derive(Debug, Serialize)]
pub struct EvalResult {
    pub schema: u32,
    pub team: String,
    pub version: String,
    /// The `[eval]` section's hash: the registry's `eval_sha256` for this
    /// version, so the result cannot be filed against other tasks.
    pub bundle_sha256: String,
    /// The model identifier every run was on.
    pub model: String,
    /// `chatty-tui <version>`.
    pub harness: String,
    /// Runs per task (k).
    pub runs: u32,
    /// RFC 3339, when the last run finished.
    pub measured_at: String,
    /// Tasks passed in every one of the `runs` runs.
    pub passed: usize,
    pub total: usize,
    pub tasks: Vec<TaskResult>,
}

#[derive(Debug, Serialize)]
pub struct TaskResult {
    pub id: String,
    /// Passed in every run.
    pub passed: bool,
    pub runs: Vec<RunResult>,
}

#[derive(Debug, Serialize)]
pub struct RunResult {
    pub passed: bool,
    /// How the team's run ended: the usage file's `exit` (`completed`,
    /// `deadline`, `error`, …), or `killed` past its hard stop.
    pub exit: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub wall_ms: u64,
}

/// Run the evaluation and write its result: to `--eval-out`, else
/// `eval-<name>-<version>.json` in the current directory.
pub async fn run(options: EvalOptions) -> Result<()> {
    let (name, version) = parse_team_ref(&options.team_ref);
    let Some(version) = version.filter(|v| !v.is_empty()) else {
        bail!(
            "--eval-team needs name@version (a score belongs to one version), \
             e.g. --eval-team payments-lead@1.0.0"
        );
    };
    if options.runs == 0 {
        bail!("--runs must be at least 1");
    }
    let spec = load_team(&name, &version)?;
    let Some(eval) = spec.eval.clone() else {
        bail!("'{name}' has no [eval] section: nothing to measure it on");
    };
    let out = options
        .out
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!("eval-{name}-{version}.json")));

    let exe = std::env::current_exe().context("cannot find the chatty-tui binary")?;
    let scratch = tempfile::Builder::new()
        .prefix("chatty-eval-")
        .tempdir()
        .context("cannot create a scratch directory")?;
    let spec_json = spec.to_json()?;
    let budget = options.max_duration.unwrap_or(DEFAULT_RUN_BUDGET) + RUN_GRACE;

    let mut model: Option<String> = None;
    let mut tasks = Vec::with_capacity(eval.tasks.len());
    for task in &eval.tasks {
        let mut runs = Vec::with_capacity(options.runs as usize);
        for run in 1..=options.runs {
            let dir = scratch.path().join(&task.id).join(format!("run-{run}"));
            let (result, run_model) = run_task(
                &exe,
                &spec,
                &spec_json,
                task,
                &dir,
                &options.forward,
                budget,
            )
            .await
            .with_context(|| format!("task '{}', run {run}", task.id))?;
            eprintln!(
                "{}: run {run}/{}: {} ({} ms)",
                task.id,
                options.runs,
                if result.passed { "pass" } else { "fail" },
                result.wall_ms
            );
            match (&model, run_model) {
                (_, None) => {}
                (None, Some(m)) => model = Some(m),
                (Some(seen), Some(m)) if *seen != m => {
                    bail!("the runs were on two models, {seen} and {m}: one score needs one model")
                }
                _ => {}
            }
            runs.push(result);
        }
        tasks.push(TaskResult {
            id: task.id.clone(),
            passed: runs.iter().all(|r| r.passed),
            runs,
        });
    }
    let Some(model) = model.filter(|m| !m.is_empty()) else {
        bail!("no run reported the model it was on; a score is never shown without one");
    };

    let result = EvalResult {
        schema: EVAL_RESULT_SCHEMA,
        team: name.clone(),
        version: version.clone(),
        bundle_sha256: eval.bundle_sha256(),
        model,
        harness: format!("chatty-tui {}", env!("CARGO_PKG_VERSION")),
        runs: options.runs,
        measured_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        passed: tasks.iter().filter(|t| t.passed).count(),
        total: tasks.len(),
        tasks,
    };
    std::fs::write(&out, serde_json::to_string_pretty(&result)? + "\n")
        .with_context(|| format!("cannot write {}", out.display()))?;
    eprintln!(
        "{name}@{version}: passed {}/{} tasks ({} run{} each), model {}. Result: {}",
        result.passed,
        result.total,
        result.runs,
        if result.runs == 1 { "" } else { "s" },
        result.model,
        out.display()
    );
    Ok(())
}

/// The team's leader spec. An installed team must be at `version`; a spec
/// that was not installed (the publisher's own) is taken as that version,
/// which the registry checks through the bundle hash.
fn load_team(name: &str, version: &str) -> Result<AgentSpec> {
    if let Some(data_dir) = dirs::data_dir() {
        let record = data_dir
            .join("chatty/installed-teams")
            .join(format!("{name}.json"));
        if let Ok(text) = std::fs::read_to_string(&record) {
            let record: TeamRecord = serde_json::from_str(&text)
                .with_context(|| format!("{} is not an install record", record.display()))?;
            if record.version != version {
                bail!(
                    "'{name}' is installed at {}, not {version}: run \
                     `chatty-tui --install-team {name}@{version}` first",
                    record.version
                );
            }
        }
    }
    let spec = load_agent_spec(name, None)
        .with_context(|| format!("--eval-team '{name}' could not be loaded"))?
        .spec;
    spec.validate(None)?;
    Ok(spec)
}

/// One run of one task in `dir`: the team, then the verifier. Returns the
/// result and the model the run reported.
async fn run_task(
    exe: &Path,
    spec: &AgentSpec,
    spec_json: &str,
    task: &EvalTask,
    dir: &Path,
    forward: &[String],
    budget: Duration,
) -> Result<(RunResult, Option<String>)> {
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace)?;
    for (path, content) in task.files.iter().flatten() {
        let file = workspace.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, content)?;
    }
    let answer = dir.join("answer.txt");
    let usage = dir.join("usage.json");

    let mut command = tokio::process::Command::new(exe);
    command
        .args(["--headless", "--auto-approve", "--agent-json", spec_json])
        .arg("--workspace")
        .arg(&workspace)
        .arg("--usage-file")
        .arg(&usage);
    if !spec.swarm.delegates_to.is_empty() {
        command.arg("--broker");
    }
    command
        .args(forward)
        .args(["-m", &task.prompt])
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&answer)?)
        .stderr(std::fs::File::create(dir.join("stderr.log"))?)
        .kill_on_drop(true);

    let started = Instant::now();
    let mut child = command.spawn().context("cannot start the team's run")?;
    let killed = match tokio::time::timeout(budget, child.wait()).await {
        Ok(status) => {
            status?;
            false
        }
        Err(_) => {
            child.kill().await.ok();
            true
        }
    };
    let wall_ms = started.elapsed().as_millis() as u64;

    let report: serde_json::Value = std::fs::read_to_string(&usage)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let exit = if killed {
        "killed".to_string()
    } else {
        report["exit"].as_str().unwrap_or("error").to_string()
    };
    let passed = verify(&task.verify, &workspace, &answer).await?;
    Ok((
        RunResult {
            passed,
            exit,
            input_tokens: report["input_tokens"].as_u64().unwrap_or(0),
            output_tokens: report["output_tokens"].as_u64().unwrap_or(0),
            wall_ms,
        },
        report["model"].as_str().map(str::to_string),
    ))
}

/// Run the task's verifier in `workspace`. Exit 0 is a pass; a verifier
/// that runs past [`VERIFY_TIMEOUT`] fails.
async fn verify(command: &str, workspace: &Path, answer: &Path) -> Result<bool> {
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(workspace)
        .env("CHATTY_EVAL_ANSWER", answer)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("cannot start the verifier (`sh`)")?;
    match tokio::time::timeout(VERIFY_TIMEOUT, child.wait()).await {
        Ok(status) => Ok(status?.success()),
        Err(_) => Ok(false),
    }
}
