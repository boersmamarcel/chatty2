//! `--export-atif` through a real `chatty-tui --headless` run (AGE-863): a
//! scripted fake model asks for real tools, the real tool loop runs them,
//! and the export must hold every model turn, tool call and result, not just
//! the question and the answer. The config each run reads is a throwaway one
//! under a temp `HOME`; nothing leaves loopback.

use std::path::Path;
use std::process::Command;

use chatty_core::agent_spec::AgentSpec;
use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use serde_json::{Value, json};

const MODEL: &str = "export-headless-model";
const README: &str = "UNIQUE-README-CONTENT";

fn write_json(dir: &Path, file: &str, value: Value) {
    std::fs::write(
        dir.join(file),
        serde_json::to_string_pretty(&value).expect("settings serialize"),
    )
    .expect("settings file");
}

/// A throwaway `HOME` whose chatty config has one model, `MODEL`, served by
/// `daemon`, and a workspace holding a README the tools can read.
fn config_for(daemon: &FakeDaemon) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a temp dir");
    let base = root.path().canonicalize().expect("the temp dir resolves");
    let workspace = base.join("workspace");
    let config = base.join("config").join("chatty");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::create_dir_all(&config).expect("config dir");
    std::fs::write(workspace.join("README.md"), README).expect("readme");

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
        workspace_dir: Some(workspace.to_string_lossy().into_owned()),
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
    root
}

/// `chatty-tui` with `args`, its whole environment under `home`.
fn chatty_tui(home: &Path, args: &[&str]) -> std::process::Output {
    let base = home.canonicalize().expect("the temp dir resolves");
    Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .args(args)
        .env("HOME", &base)
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_RUNTIME_DIR", base.join("run"))
        .env("CHATTY_SECRET_STORE", "file")
        .output()
        .expect("chatty-tui runs")
}

/// What one headless run left behind: the export and the saved history.
struct Exported {
    atif: Value,
    history: Vec<Value>,
}

/// A headless run that lists the workspace, reads the README, then answers.
fn run_with_two_tool_calls() -> Exported {
    let daemon = FakeDaemon::scripted(Script::new().route(
        MODEL,
        [
            Reply::tool_call("list_directory", json!({ "path": "." })),
            Reply::tool_call("read_file", json!({ "path": "README.md" })),
            Reply::text("The README says UNIQUE-README-CONTENT."),
        ],
    ));
    let home = config_for(&daemon);
    let export = home.path().join("run.atif.json");
    let saved = home.path().join("run.json");

    let mut spec = AgentSpec::named("export-user");
    spec.agent.model = Some(MODEL.to_string());
    let spec = serde_json::to_string(&spec).unwrap();
    let output = chatty_tui(
        home.path(),
        &[
            "--headless",
            "-m",
            "What does the README say?",
            "--agent-json",
            &spec,
            "--export-atif",
            export.to_str().unwrap(),
            "--save-conversation",
            saved.to_str().unwrap(),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "chatty-tui failed\nstdout:\n{}\nstderr:\n{stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(daemon.requests().len(), 3, "two tool calls, then the answer");

    let atif = serde_json::from_str(&std::fs::read_to_string(&export).expect("the export"))
        .expect("the export is JSON");
    let history = serde_json::from_str(&std::fs::read_to_string(&saved).expect("the history"))
        .expect("the history is JSON");
    Exported { atif, history }
}

fn steps(atif: &Value) -> &Vec<Value> {
    atif["steps"].as_array().expect("steps")
}

#[test]
fn headless_export_includes_tool_calls_from_a_real_tool_loop() {
    let Exported { atif, .. } = run_with_two_tool_calls();
    let steps = steps(&atif);

    let calls: Vec<&Value> = steps
        .iter()
        .flat_map(|s| s["tool_calls"].as_array().into_iter().flatten())
        .collect();
    let names: Vec<&str> = calls
        .iter()
        .map(|c| c["function_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["list_directory", "read_file"], "{atif:#}");
    assert_eq!(calls[1]["arguments"]["path"], "README.md", "{atif:#}");

    // Every call has its result, joined to it by id.
    for step in steps.iter().filter(|s| s["tool_calls"].is_array()) {
        let call_id = &step["tool_calls"][0]["tool_call_id"];
        let result = &step["observation"]["results"][0];
        assert_eq!(&result["source_call_id"], call_id, "{atif:#}");
        assert!(result["content"].is_string(), "{atif:#}");
    }
    let read = steps
        .iter()
        .find(|s| s["tool_calls"][0]["function_name"] == "read_file")
        .unwrap();
    assert!(
        read["observation"]["results"][0]["content"]
            .as_str()
            .unwrap()
            .contains(README),
        "{atif:#}"
    );
    assert!(
        steps
            .last()
            .unwrap()
            .to_string()
            .contains("The README says UNIQUE-README-CONTENT."),
        "{atif:#}"
    );
}

#[test]
fn headless_export_step_count_matches_transcript() {
    let Exported { atif, history } = run_with_two_tool_calls();
    let steps = steps(&atif);

    // The question, one step per model turn (two tool calls, the answer);
    // tool results join the step whose call they answer.
    let tool_results = history
        .iter()
        .filter(|m| m.to_string().contains("\"tool_result\"") || m.to_string().contains("toolresult"))
        .count();
    assert_eq!(
        steps.len(),
        history.len() - tool_results,
        "history: {history:#?}\nexport: {atif:#}"
    );
    assert_eq!(steps.len(), 4, "{atif:#}");
    let sources: Vec<&str> = steps.iter().map(|s| s["source"].as_str().unwrap()).collect();
    assert_eq!(sources, ["user", "agent", "agent", "agent"], "{atif:#}");
    let ids: Vec<u64> = steps.iter().map(|s| s["step_id"].as_u64().unwrap()).collect();
    assert_eq!(ids, [1, 2, 3, 4], "{atif:#}");
}
