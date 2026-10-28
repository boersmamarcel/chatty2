//! AGE-918, through a real `chatty-tui --headless` against a scripted fake
//! model: a `bwrap` that fails the way Ubuntu 24.04's AppArmor makes it fail
//! is said once on stderr, with the fix, and `sandbox_required` ends the run
//! with an error instead. The config is a throwaway one under a temp `HOME`.
#![cfg(target_os = "linux")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};

const MODEL: &str = "sandbox-headless-model";

fn write_json(dir: &Path, file: &str, value: serde_json::Value) {
    std::fs::write(
        dir.join(file),
        serde_json::to_string_pretty(&value).unwrap(),
    )
    .unwrap();
}

/// A throwaway `HOME` with one model served by `daemon`.
fn config_for(daemon: &FakeDaemon, sandbox_required: bool) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let workspace = base.join("workspace");
    let config = base.join("config").join("chatty");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&config).unwrap();
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
        sandbox_required,
        ..ExecutionSettingsModel::default()
    };
    write_json(&config, "providers.json", serde_json::json!([provider]));
    write_json(&config, "models.json", serde_json::json!([model]));
    write_json(
        &config,
        "execution_settings.json",
        serde_json::to_value(&execution).unwrap(),
    );
    root
}

/// Run headless with a `bwrap` first on `PATH` that fails like AppArmor does.
fn run(home: &Path) -> std::process::Output {
    let fake_bin = tempfile::tempdir().unwrap();
    let bwrap = fake_bin.path().join("bwrap");
    std::fs::write(
        &bwrap,
        "#!/bin/sh\necho 'bwrap: setting up uid map: Permission denied' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let base = home.canonicalize().unwrap();
    Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .args([
            "--headless",
            "-m",
            "Run echo sandbox-ok.",
            "--max-duration",
            "120",
            "--enable",
            "shell",
        ])
        .env("PATH", path)
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

fn two_step_script() -> Script {
    Script::new().route(
        MODEL,
        [
            Reply::tool_call(
                "shell_execute",
                serde_json::json!({ "command": "echo sandbox-ok" }),
            ),
            Reply::text("Done."),
        ],
    )
}

#[test]
fn an_unavailable_sandbox_is_said_once_on_stderr() {
    let daemon = FakeDaemon::scripted(two_step_script());
    let home = config_for(&daemon, false);
    let output = run(home.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(
        stderr.matches("shell: sandbox unavailable (").count(),
        1,
        "{stderr}"
    );
    let line = stderr
        .lines()
        .find(|l| l.contains("shell: sandbox unavailable"))
        .unwrap();
    assert!(line.contains("setting up uid map"), "{line}");
    assert!(line.contains("commands run on the host"), "{line}");
    assert_eq!(daemon.requests().len(), 2, "the default turn is unchanged");
}

#[test]
fn sandbox_required_fails_the_run_with_a_clear_error() {
    let daemon = FakeDaemon::scripted(two_step_script());
    let home = config_for(&daemon, true);
    let output = run(home.path());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("sandbox required but unavailable")
            && stderr.contains("setting up uid map"),
        "{stderr}"
    );
    assert!(daemon.requests().is_empty(), "no model call was made");
}
