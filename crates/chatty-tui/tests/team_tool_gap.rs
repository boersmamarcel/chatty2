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
const DEV_MODEL: &str = "tool-gap-dev-model";

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).expect("parent dir");
    std::fs::write(path, text).expect("file");
}

/// stderr of a headless `--team devteam` run (leader `coordinator`, one
/// `coder` worker) whose execution_settings.json has shell and git `file_on`,
/// started with `extra` flags.
fn team_stderr(file_on: bool, extra: &[&str]) -> String {
    let daemon = FakeDaemon::scripted(Script::new().route(MODEL, [Reply::text("done")]));
    let stderr = team_run(&daemon, file_on, extra);
    assert_eq!(daemon.requests().len(), 1, "the run itself is unchanged");
    stderr
}

/// As [`team_stderr`] over a given daemon: the lead and the developer run
/// different models (`MODEL`, `DEV_MODEL`), so a script can tell their
/// requests apart.
fn team_run(daemon: &FakeDaemon, file_on: bool, extra: &[&str]) -> String {
    let root = tempfile::tempdir().expect("a temp dir");
    let base = root.path().canonicalize().expect("the temp dir resolves");
    let workspace = base.join("workspace");
    let config = base.join("config").join("chatty");

    let provider = ProviderConfig::new("Fake".to_string(), ProviderType::Ollama)
        .with_base_url(daemon.base_url());
    let models: Vec<ModelConfig> = [MODEL, DEV_MODEL]
        .iter()
        .map(|m| {
            ModelConfig::new(
                m.to_string(),
                m.to_string(),
                ProviderType::Ollama,
                m.to_string(),
            )
        })
        .collect();
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
    write(&config.join("models.json"), &json!(models).to_string());
    write(
        &config.join("execution_settings.json"),
        &serde_json::to_string(&execution).unwrap(),
    );
    let agents = workspace.join(".chatty/agents");
    write(
        &agents.join("lead.toml"),
        &format!(
            "[agent]\nname = \"lead\"\nmodel = \"{MODEL}\"\n[tools]\nprofile = \"coordinator\"\n[swarm]\ndelegates_to = [\"developer\"]\n"
        ),
    );
    write(
        &agents.join("developer.toml"),
        &format!(
            "[agent]\nname = \"developer\"\nmodel = \"{DEV_MODEL}\"\n[tools]\nprofile = \"coder\"\n"
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
    stderr.into_owned()
}

#[test]
fn a_headless_team_with_a_coder_and_shell_off_warns_before_the_first_turn() {
    let stderr = team_stderr(false, &[]);
    assert!(
        stderr.contains(
            "team devteam: developer's profile 'coder' wants shell and git, but shell is \
             disabled (execution_settings.enabled=false) and git is disabled \
             (execution_settings.git_enabled=false); enable with --enable shell,git"
        ),
        "stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("lead's profile"),
        "a coordinator wants neither: {stderr}"
    );
}

#[test]
fn enable_on_the_command_line_reaches_the_workers_so_nothing_is_missing() {
    let stderr = team_stderr(false, &["--enable", "shell,git"]);
    assert!(!stderr.contains("team devteam:"), "{stderr}");
}

#[test]
fn disable_on_the_command_line_warns_for_the_workers_too() {
    let stderr = team_stderr(true, &["--disable", "shell"]);
    assert!(
        stderr.contains("developer's profile 'coder' wants shell, but shell is disabled"),
        "{stderr}"
    );
}

/// AGE-924: `--enable` on the leader is forwarded to its workers. The lead
/// delegates to the developer; the developer is a separate `chatty-tui`
/// process that reads execution_settings.json (shell off) itself, so it is
/// offered `shell_execute` only because the flag rode along.
#[test]
fn enable_on_the_command_line_gives_the_worker_a_shell() {
    let offered = |extra: &[&str]| {
        let daemon = FakeDaemon::scripted(
            Script::new()
                .route(
                    MODEL,
                    [
                        Reply::tool_call(
                            "invoke_agent",
                            json!({ "agent": "developer", "prompt": "list the files" }),
                        ),
                        Reply::text("done"),
                    ],
                )
                .route(DEV_MODEL, [Reply::text("listed")]),
        );
        team_run(&daemon, false, extra);
        let requests = daemon.requests_for(DEV_MODEL);
        assert_eq!(requests.len(), 1, "the developer ran once");
        String::from_utf8_lossy(&requests[0].body).contains("shell_execute")
    };
    assert!(
        offered(&["--enable", "shell,git"]),
        "the worker has a shell"
    );
    assert!(!offered(&[]), "without the flag the file decides: no shell");
}
