//! AGE-922: a resumed headless `--team` leader opens its first turn with a
//! resume brief; a first run and `--resume-brief off` do not. Real
//! `chatty-tui` runs against a fake model, over one git workspace that keeps
//! its session record between runs.

use std::path::Path;
use std::process::Command;

use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use serde_json::json;

const MODEL: &str = "resume-brief-model";
const BRIEF: &str = "RESUME BRIEF";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("parent dir");
    std::fs::write(path, text).expect("file");
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repository like the coding run left it: the S005 code merged into
/// `main`, its tests on an unmerged branch that is checked out, and a second
/// unmerged branch. The team is a `lead` with one worker.
fn workspace(base: &Path) -> std::path::PathBuf {
    let ws = base.join("workspace");
    std::fs::create_dir_all(&ws).unwrap();
    git(&ws, &["init", "-q", "-b", "main"]);
    git(&ws, &["config", "user.email", "t@example.com"]);
    git(&ws, &["config", "user.name", "t"]);
    git(&ws, &["config", "commit.gpgsign", "false"]);
    write(&ws.join("app.py"), "def s005(): pass\n");
    git(&ws, &["add", "app.py"]);
    git(&ws, &["commit", "-q", "-m", "S005 code"]);
    git(&ws, &["checkout", "-q", "-b", "feature/s006"]);
    git(&ws, &["commit", "-q", "--allow-empty", "-m", "Start S006"]);
    git(&ws, &["checkout", "-q", "main"]);
    git(&ws, &["checkout", "-q", "-b", "feature/s005-tests"]);
    git(
        &ws,
        &["commit", "-q", "--allow-empty", "-m", "Add S005 tests"],
    );
    let agents = ws.join(".chatty/agents");
    write(
        &agents.join("lead.toml"),
        &format!(
            "[agent]\nname = \"lead\"\nmodel = \"{MODEL}\"\n[tools]\nprofile = \"coordinator\"\n[swarm]\ndelegates_to = [\"developer\"]\n"
        ),
    );
    write(
        &agents.join("developer.toml"),
        &format!("[agent]\nname = \"developer\"\nmodel = \"{MODEL}\"\n"),
    );
    write(
        &ws.join(".chatty/teams/devteam/team.json"),
        r#"{"leader":"lead","agents":["developer"]}"#,
    );
    ws
}

/// One `--team devteam --headless` run in `ws`, under its own throwaway
/// config whose model answers "done"; the text of the leader's first
/// request.
fn first_request(base: &Path, run: &str, ws: &Path, extra: &[&str]) -> String {
    let (output, body) = run_once(base, run, ws, extra, Reply::text("done"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "chatty-tui failed\nstderr:\n{stderr}"
    );
    body
}

/// One `--team devteam --headless` run in `ws` whose model answers with
/// `reply`: the process's output and the text of its one model request.
fn run_once(
    base: &Path,
    run: &str,
    ws: &Path,
    extra: &[&str],
    reply: Reply,
) -> (std::process::Output, String) {
    let daemon = FakeDaemon::scripted(Script::new().route(MODEL, [reply]));
    let home = base.join(run);
    let config = home.join("config").join("chatty");
    let provider = ProviderConfig::new("Fake".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());
    let model = ModelConfig::new(
        MODEL.to_string(),
        MODEL.to_string(),
        ProviderType::Ollama,
        MODEL.to_string(),
    );
    let execution = ExecutionSettingsModel {
        workspace_dir: Some(ws.to_string_lossy().into_owned()),
        fetch_enabled: false,
        memory_enabled: false,
        approval_mode: ApprovalMode::AutoApproveAll,
        ..ExecutionSettingsModel::default()
    };
    write(
        &config.join("providers.json"),
        &json!([provider]).to_string(),
    );
    write(&config.join("models.json"), &json!([model]).to_string());
    write(
        &config.join("execution_settings.json"),
        &serde_json::to_string(&execution).unwrap(),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .args(["--headless", "-m", "Work the backlog.", "--team", "devteam"])
        .args(extra)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("CHATTY_SECRET_STORE", "file")
        .output()
        .expect("chatty-tui runs");
    let requests = daemon.requests();
    assert_eq!(requests.len(), 1, "one model call per run");
    (
        output,
        String::from_utf8_lossy(&requests[0].body).into_owned(),
    )
}

/// AGE-903 × AGE-922: the endpoint refusing the leader's own call on a spent
/// budget fails the run, and the record says `budget`, not `error`.
#[test]
fn a_402_budget_refusal_on_the_leader_records_budget() {
    let root = tempfile::tempdir().expect("a temp dir");
    let base = root.path().canonicalize().unwrap();
    let ws = workspace(&base);
    let (output, _) = run_once(
        &base,
        "run1",
        &ws,
        &[],
        Reply::ErrorBody(
            402,
            json!({ "error": "cap_exceeded", "month_to_date": 12.5, "cap_usd": 10.0 }),
        ),
    );
    assert!(!output.status.success(), "a budget stop fails the run");
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(ws.join(".chatty/team-state/devteam.json")).expect("the record"),
    )
    .unwrap();
    assert_eq!(record["outcome"], "budget", "{record}");
}

#[test]
fn a_resumed_team_run_opens_with_the_brief_and_a_first_run_or_off_does_not() {
    let root = tempfile::tempdir().expect("a temp dir");
    let base = root.path().canonicalize().unwrap();
    let ws = workspace(&base);
    let record = ws.join(".chatty/team-state/devteam.json");

    // A first run: no record, so nothing is added.
    let first = first_request(&base, "run1", &ws, &[]);
    assert!(
        !first.contains(BRIEF),
        "a first run is not briefed: {first}"
    );
    assert!(first.contains("Work the backlog."), "{first}");
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record).expect("the record")).unwrap();
    assert_eq!(written["outcome"], "completed", "{written}");
    assert_eq!(written["final_answer"], "done", "{written}");
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&status.stdout).contains("team-state"),
        "the record stays out of git status"
    );

    // The previous lease ran out of time: the record a host would supply.
    std::fs::write(
        &record,
        r#"{"schema":1,"outcome":"deadline","final_answer":"S005 reviewed; tests still on feature/s005-tests.","open_todos":[{"title":"Merge the S005 tests","status":"in_progress"}]}"#,
    )
    .unwrap();
    let resumed = first_request(&base, "run2", &ws, &[]);
    for fact in [
        BRIEF,
        "checked out on `feature/s005-tests`",
        "`feature/s005-tests`: 1 commit ahead, latest",
        "`feature/s006`: 1 commit ahead, latest",
        "(deadline)",
        "[in progress] Merge the S005 tests",
        "> S005 reviewed; tests still on feature/s005-tests.",
        "Resume, do not restart",
    ] {
        assert!(resumed.contains(fact), "missing {fact:?}: {resumed}");
    }
    let brief_at = resumed.find(BRIEF).unwrap();
    let task_at = resumed.find("Work the backlog.").unwrap();
    assert!(brief_at < task_at, "the brief comes before the task");

    let off = first_request(&base, "run3", &ws, &["--resume-brief", "off"]);
    assert!(!off.contains(BRIEF), "off is never briefed: {off}");
    assert!(
        std::fs::read_to_string(&record)
            .unwrap()
            .contains("\"outcome\":\"completed\""),
        "the record is still written under off"
    );
}
