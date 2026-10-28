//! AGE-921: a headless `--team` run whose agents' profiles want the shell or
//! git while the execution settings have them off says so on stderr before
//! the first model turn. Only a warning: the run goes on unchanged.

use std::path::Path;
use std::process::Command;

use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use serde_json::json;

const MODEL: &str = "tool-gap-model";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("parent dir");
    std::fs::write(path, text).expect("file");
}

/// stderr of a headless `--team devteam` run (leader `coordinator`, one
/// `coder` worker) whose execution_settings.json has shell and git `file_on`,
/// started with `extra` flags.
fn team_stderr(file_on: bool, extra: &[&str]) -> String {
    let daemon = FakeDaemon::scripted(Script::new().route(MODEL, [Reply::text("done")]));
    let root = tempfile::tempdir().expect("a temp dir");
    let base = root.path().canonicalize().expect("the temp dir resolves");
    let workspace = base.join("workspace");
    let config = base.join("config").join("chatty");

    let provider = ProviderConfig::new("Fake".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());
    let model = ModelConfig::new(
        MODEL.to_string(),
        MODEL.to_string(),
        ProviderType::Ollama,
        MODEL.to_string(),
    );
    let execution = ExecutionSettingsModel {
        enabled: file_on,
        git_enabled: file_on,
        workspace_dir: Some(workspace.to_string_lossy().into_owned()),
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
    let agents = workspace.join(".chatty/agents");
    write(
        &agents.join("lead.toml"),
        &format!(
            "[agent]\nname = \"lead\"\nmodel = \"{MODEL}\"\n[tools]\nprofile = \"coordinator\"\n"
        ),
    );
    write(
        &agents.join("developer.toml"),
        &format!(
            "[agent]\nname = \"developer\"\nmodel = \"{MODEL}\"\n[tools]\nprofile = \"coder\"\n"
        ),
    );
    write(
        &workspace.join(".chatty/teams/devteam/team.json"),
        r#"{"leader":"lead","agents":["developer"]}"#,
    );

    let output = Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .args(["--headless", "-m", "go", "--team", "devteam"])
        .args(extra)
        .env("HOME", &base)
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_RUNTIME_DIR", base.join("run"))
        .env("CHATTY_SECRET_STORE", "file")
        .output()
        .expect("chatty-tui runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "chatty-tui failed\nstderr:\n{stderr}"
    );
    assert_eq!(daemon.requests().len(), 1, "the run itself is unchanged");
    stderr.into_owned()
}

#[test]
fn a_headless_team_with_a_coder_and_shell_off_warns_before_the_first_turn() {
    let stderr = team_stderr(false, &[]);
    assert!(
        stderr.contains(
            "team devteam: developer's profile 'coder' wants shell and git, but shell is \
             disabled (execution_settings.json enabled=false) and git is disabled \
             (execution_settings.json git_enabled=false); set shell and git in \
             execution_settings.json (workers do not inherit --enable)"
        ),
        "stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("lead's profile"),
        "a coordinator wants neither: {stderr}"
    );
}

#[test]
fn enable_on_the_command_line_does_not_reach_the_workers() {
    let stderr = team_stderr(false, &["--enable", "shell,git"]);
    assert!(
        stderr.contains("developer's profile 'coder' wants shell and git"),
        "{stderr}"
    );
}

#[test]
fn disable_on_the_command_line_warns_for_no_worker_that_the_file_enables() {
    let stderr = team_stderr(true, &["--disable", "shell"]);
    assert!(!stderr.contains("team devteam:"), "{stderr}");
}
