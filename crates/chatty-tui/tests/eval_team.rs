//! `chatty-tui --eval-team` end to end on the fake model (MK-T6, AGE-851):
//! a team spec with an `[eval]` section, installed in a throwaway data
//! directory, runs each task k times through real headless children; the
//! task verifiers run for real and decide pass or fail; the result JSON
//! carries per-task pass/fail, tokens, wall-clock, the model, the harness
//! version and the bundle hash. Nothing leaves loopback.
#![cfg(unix)]

use std::path::Path;
use std::process::Command;

use chatty_core::agent_spec::AgentSpec;
use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use serde_json::{Value, json};

const MODEL: &str = "eval-team-model";

const SPEC: &str = r#"
[agent]
name = "iban-team"
description = "Checks a payment run"
model = "eval-team-model"

[[eval.tasks]]
id = "hold-bad-iban"
prompt = "Which payment in payments.csv must be held?"
verify = "grep -q NL00BANK0123456789 \"$CHATTY_EVAL_ANSWER\""
files = { "payments.csv" = "vendor,iban,amount\nAcme,NL00BANK0123456789,120.00\n" }

[[eval.tasks]]
id = "write-report"
prompt = "Write report.txt listing the payments to hold."
verify = "test -s report.txt"
"#;

fn write_json(dir: &Path, file: &str, value: Value) {
    std::fs::write(dir.join(file), serde_json::to_string_pretty(&value).unwrap()).unwrap();
}

/// A throwaway `HOME`: one model served by `daemon`, and the team spec
/// installed in the data directory, where `--agent` finds it.
fn home_for(daemon: &FakeDaemon) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let config = base.join("config").join("chatty");
    let agents = base.join("data").join("chatty").join("agents");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&agents).unwrap();
    let provider = ProviderConfig::new("Fake".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());
    let model = ModelConfig::new(
        MODEL.to_string(),
        MODEL.to_string(),
        ProviderType::Ollama,
        MODEL.to_string(),
    );
    let execution = ExecutionSettingsModel {
        enabled: true,
        fetch_enabled: false,
        memory_enabled: false,
        approval_mode: ApprovalMode::AutoApproveAll,
        ..ExecutionSettingsModel::default()
    };
    write_json(&config, "providers.json", json!([provider]));
    write_json(&config, "models.json", json!([model]));
    write_json(
        &config,
        "execution_settings.json",
        serde_json::to_value(&execution).unwrap(),
    );
    std::fs::write(agents.join("iban-team.toml"), SPEC).unwrap();
    root
}

fn chatty_tui(home: &Path, args: &[&str]) -> std::process::Output {
    let base = home.canonicalize().unwrap();
    Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .args(args)
        .current_dir(&base)
        .env("HOME", &base)
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_RUNTIME_DIR", base.join("run"))
        .output()
        .expect("chatty-tui runs")
}

/// One scripted answer with the usage it reports.
fn answer(reply: Reply) -> [Reply; 2] {
    [
        Reply::Usage {
            input: 100,
            output: 10,
            cache_read: 0,
        },
        reply,
    ]
}

#[test]
fn eval_team_runs_verifier_and_reports() {
    // The runs go in order: hold-bad-iban twice, then write-report twice.
    // The bad IBAN is named both times; the report is written only in the
    // first write-report run, so its verifier fails the second.
    let replies: Vec<Reply> = [
        Reply::text("Hold Acme: NL00BANK0123456789 fails the mod-97 check."),
        Reply::text("Hold NL00BANK0123456789 (Acme, 120.00)."),
        Reply::tool_call(
            "write_file",
            json!({ "path": "report.txt", "content": "Hold: Acme 120.00\n" }),
        ),
        Reply::text("Wrote report.txt."),
        Reply::text("Nothing to write; hold Acme."),
    ]
    .into_iter()
    .flat_map(answer)
    .collect();
    let daemon = FakeDaemon::scripted(Script::new().route(MODEL, replies));
    let home = home_for(&daemon);
    let out = home.path().join("result.json");

    let output = chatty_tui(
        home.path(),
        &[
            "--eval-team",
            "iban-team@1.0.0",
            "--runs",
            "2",
            "--model",
            MODEL,
            "--eval-out",
            out.to_str().unwrap(),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "--eval-team failed:\n{stderr}");
    assert_eq!(daemon.requests().len(), 5, "every scripted turn ran\n{stderr}");
    assert!(
        stderr.contains("passed 1/2 tasks (2 runs each), model eval-team-model"),
        "{stderr}"
    );

    let result: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let spec = AgentSpec::from_toml(SPEC).unwrap();
    assert_eq!(result["schema"], 1);
    assert_eq!(result["team"], "iban-team");
    assert_eq!(result["version"], "1.0.0");
    assert_eq!(
        result["bundle_sha256"],
        spec.eval.unwrap().bundle_sha256(),
        "the result names the tasks it was measured on"
    );
    assert_eq!(result["model"], MODEL);
    assert_eq!(
        result["harness"],
        format!("chatty-tui {}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(result["runs"], 2);
    assert!(
        chrono_like(result["measured_at"].as_str().unwrap()),
        "{result}"
    );
    assert_eq!(result["passed"], 1);
    assert_eq!(result["total"], 2);

    let tasks = result["tasks"].as_array().unwrap();
    assert_eq!(tasks[0]["id"], "hold-bad-iban");
    assert_eq!(tasks[0]["passed"], true);
    assert_eq!(tasks[1]["id"], "write-report");
    assert_eq!(tasks[1]["passed"], false, "a task passes only in every run");
    let passes: Vec<bool> = tasks
        .iter()
        .flat_map(|t| t["runs"].as_array().unwrap())
        .map(|r| r["passed"].as_bool().unwrap())
        .collect();
    assert_eq!(passes, [true, true, true, false], "{result}");
    let run = &tasks[1]["runs"][0];
    assert_eq!(run["exit"], "completed");
    assert_eq!(run["input_tokens"], 200, "two model calls, 100 each");
    assert_eq!(run["output_tokens"], 20);
    assert!(run["wall_ms"].as_u64().is_some(), "{run}");
    assert_eq!(tasks[0]["runs"][0]["input_tokens"], 100);

    // The fixture file reached the agent's workspace, and the prompt its
    // first message.
    let first = daemon.requests()[0].json().to_string();
    assert!(first.contains("Which payment in payments.csv must be held?"));
}

#[test]
fn eval_team_needs_a_version_and_an_eval_section() {
    let daemon = FakeDaemon::scripted(Script::new());
    let home = home_for(&daemon);
    let output = chatty_tui(home.path(), &["--eval-team", "iban-team", "--runs", "1"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("name@version"));

    let agents = home.path().join("data/chatty/agents");
    std::fs::write(
        agents.join("plain.toml"),
        "[agent]\nname = \"plain\"\n",
    )
    .unwrap();
    let output = chatty_tui(home.path(), &["--eval-team", "plain@1.0.0"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no [eval] section"));

    // An installed team is measured at the version installed, no other.
    let installed = home.path().join("data/chatty/installed-teams");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::write(
        installed.join("iban-team.json"),
        json!({
            "leader": "iban-team", "version": "1.1.0", "author": "ada",
            "registry_url": "http://127.0.0.1:1", "specs": ["iban-team"],
            "plugins": [], "installed_plugins": []
        })
        .to_string(),
    )
    .unwrap();
    let output = chatty_tui(home.path(), &["--eval-team", "iban-team@1.0.0"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("installed at 1.1.0, not 1.0.0"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(daemon.requests().is_empty(), "no run started");
}

/// `2026-10-04T07:50:43Z`: RFC 3339, seconds, UTC.
fn chrono_like(text: &str) -> bool {
    text.len() == 20 && text.ends_with('Z') && text.as_bytes()[10] == b'T'
}
