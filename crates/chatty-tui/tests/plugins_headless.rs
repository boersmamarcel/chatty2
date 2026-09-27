//! PL-U2 (AGE-616), end to end: a real `chatty-tui --headless` running a
//! spec that lists the echo plugin, against a scripted fake model, calls the
//! plugin's `reverse` tool in-process and hands the reversed string back to
//! the model. The config it reads is a throwaway one under a temp `HOME`;
//! nothing leaves loopback.

use std::path::{Path, PathBuf};
use std::process::Command;

use chatty_core::agent_spec::{AgentSpec, PluginSpec};
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, Reply, Script};
use chatty_wasm_runtime::test_support::fixture_path;

const MODEL: &str = "plugin-headless-model";

/// `target/wasm-fixtures`, where every staged module is a directory with
/// its own `module.toml`: the host's module directory for this run.
fn module_dir() -> PathBuf {
    fixture_path("echo-agent")
        .parent()
        .and_then(Path::parent)
        .expect("fixtures live in target/wasm-fixtures/<name>/")
        .to_path_buf()
}

fn write_json(dir: &Path, file: &str, value: serde_json::Value) {
    std::fs::write(
        dir.join(file),
        serde_json::to_string_pretty(&value).expect("settings serialize"),
    )
    .expect("settings file");
}

#[test]
fn headless_spec_calls_the_echo_plugin() {
    let daemon = FakeDaemon::scripted(Script::new().route(
        MODEL,
        [
            Reply::tool_call(
                "echo-agent__reverse",
                serde_json::json!({ "input": "hello" }),
            ),
            Reply::text("The reversed string is olleh."),
        ],
    ));

    let root = tempfile::tempdir().expect("a temp dir");
    let base = root.path().canonicalize().expect("the temp dir resolves");
    let workspace = base.join("workspace");
    let config = base.join("config").join("chatty");
    std::fs::create_dir_all(&workspace).expect("workspace dir");
    std::fs::create_dir_all(&config).expect("config dir");

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
    let modules = ModuleSettingsModel {
        module_dir: module_dir().to_string_lossy().into_owned(),
        ..ModuleSettingsModel::default()
    };
    write_json(&config, "providers.json", serde_json::json!([provider]));
    write_json(&config, "models.json", serde_json::json!([model]));
    write_json(
        &config,
        "execution_settings.json",
        serde_json::to_value(&execution).unwrap(),
    );
    write_json(
        &config,
        "module_settings.json",
        serde_json::to_value(&modules).unwrap(),
    );

    let mut spec = AgentSpec::named("echo-user");
    spec.agent.model = Some(MODEL.to_string());
    spec.tools.profile = Some("coordinator".to_string());
    spec.plugins = vec![PluginSpec {
        module: "echo-agent".to_string(),
        ..PluginSpec::default()
    }];

    let output = Command::new(env!("CARGO_BIN_EXE_chatty-tui"))
        .args(["--headless", "-m", "Reverse hello with the echo plugin."])
        .arg("--agent-json")
        .arg(serde_json::to_string(&spec).unwrap())
        .env("HOME", &base)
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_DATA_HOME", base.join("data"))
        .env("XDG_CACHE_HOME", base.join("cache"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_RUNTIME_DIR", base.join("run"))
        .output()
        .expect("chatty-tui runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "chatty-tui failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let requests = daemon.requests();
    assert_eq!(requests.len(), 2, "a tool call, then the answer\n{stderr}");
    let tools: Vec<String> = requests[0].json()["tools"]
        .as_array()
        .expect("the first request advertises tools")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
        .collect();
    assert!(
        tools.contains(&"echo-agent__reverse".to_string()),
        "{tools:?}"
    );
    assert!(
        String::from_utf8_lossy(&requests[1].body).contains("olleh"),
        "the plugin's result reaches the model"
    );
    assert!(stdout.contains("olleh"), "{stdout}");
}
