//! Plugins end to end, through a real `chatty-tui --headless` against a
//! scripted fake model: PL-U2 (AGE-616), a spec that lists the echo plugin
//! calls its `reverse` tool in-process and hands the reversed string back to
//! the model; PL-U3 (AGE-618), the `benford-analyst` preset (a spec plus the
//! `benford` plugin, benford's old agent loop gone) answers the tutorial's
//! dataset with the right chi-square verdict. The config each run reads is a
//! throwaway one under a temp `HOME`; nothing leaves loopback.

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
    fixture_path("echo")
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

/// A throwaway `HOME` whose chatty config has one model, `MODEL`, served by
/// `daemon`, and the staged fixtures as its module directory.
fn config_for(daemon: &FakeDaemon) -> tempfile::TempDir {
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
        .output()
        .expect("chatty-tui runs")
}

#[test]
fn headless_spec_calls_the_echo_plugin() {
    let daemon = FakeDaemon::scripted(Script::new().route(
        MODEL,
        [
            Reply::tool_call(
                "echo__reverse",
                serde_json::json!({ "input": "hello" }),
            ),
            Reply::text("The reversed string is olleh."),
        ],
    ));
    let home = config_for(&daemon);

    let mut spec = AgentSpec::named("echo-user");
    spec.agent.model = Some(MODEL.to_string());
    spec.tools.profile = Some("coordinator".to_string());
    spec.plugins = vec![PluginSpec {
        module: "echo".to_string(),
        ..PluginSpec::default()
    }];

    let spec = serde_json::to_string(&spec).unwrap();
    let output = chatty_tui(
        home.path(),
        &[
            "--headless",
            "-m",
            "Reverse hello with the echo plugin.",
            "--agent-json",
            &spec,
        ],
    );
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
        tools.contains(&"echo__reverse".to_string()),
        "{tools:?}"
    );
    assert!(
        String::from_utf8_lossy(&requests[1].body).contains("olleh"),
        "the plugin's result reaches the model"
    );
    assert!(stdout.contains("olleh"), "{stdout}");
}

/// The tool results a request carries back to the model, parsed as JSON
/// (the benford tools answer JSON).
fn tool_results(request: &serde_json::Value) -> Vec<serde_json::Value> {
    request["messages"]
        .as_array()
        .expect("a chat request has messages")
        .iter()
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| m["content"].as_str())
        .filter_map(|content| serde_json::from_str(content).ok())
        .collect()
}

/// PL-U3's verification: `chatty-tui --agent benford-analyst --headless`
/// answers the tutorial's dataset question. The model is scripted to call
/// both tools the preamble names; everything else is real — the preset spec,
/// the `benford` plugin (0.3.0, loaded in-process), and its arithmetic. The
/// verdict the model is handed is the plugin's: first digits
/// 1,4,8,2,5,8,2,4,7 give χ² ≈ 10.49 on 8 degrees of freedom, under the
/// 15.507 critical value, so LOW risk, digit 1 the most deviant.
#[test]
fn benford_analyst_preset_gives_the_chi_square_verdict() {
    const DATASET: [f64; 9] = [
        1234.0, 4521.0, 891.0, 2340.0, 567.0, 8901.0, 234.0, 456.0, 789.0,
    ];
    let counts = [1, 2, 0, 2, 1, 0, 1, 2, 0];
    let daemon = FakeDaemon::scripted(Script::new().route(
        MODEL,
        [
            Reply::tool_call(
                "benford__compute_benford_distribution",
                serde_json::json!({ "numbers": DATASET }),
            ),
            Reply::tool_call(
                "benford__chi_square_test",
                serde_json::json!({ "observed_counts": counts, "total": 9 }),
            ),
            Reply::text("Risk level: LOW (chi-square 10.49, df 8)."),
        ],
    ));
    let home = config_for(&daemon);

    let output = chatty_tui(
        home.path(),
        &[
            "--agent",
            "benford-analyst",
            "--headless",
            "-m",
            "Analyze these invoice amounts: 1234 4521 891 2340 567 8901 234 456 789",
        ],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "chatty-tui failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let requests = daemon.requests();
    assert_eq!(
        requests.len(),
        3,
        "two tool calls, then the report\n{stderr}"
    );
    let first = requests[0].json();
    let tools: Vec<&str> = first["tools"]
        .as_array()
        .expect("the first request advertises tools")
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    for tool in [
        "benford__compute_benford_distribution",
        "benford__chi_square_test",
    ] {
        assert!(tools.contains(&tool), "{tool} in {tools:?}");
    }
    assert!(
        !tools.contains(&"shell_execute"),
        "the preset has no shell: {tools:?}"
    );
    assert!(
        first.to_string().contains("forensic financial auditor"),
        "the preset's preamble is the system prompt"
    );

    // The distribution the plugin computed is the one the model then tests.
    let distribution = tool_results(&requests[1].json());
    assert_eq!(distribution.len(), 1, "{distribution:?}");
    assert_eq!(distribution[0]["total_analyzed"], 9);
    assert_eq!(distribution[0]["observed_counts"], serde_json::json!(counts));

    // The verdict: the plugin's own chi-square test, handed to the model.
    let results = tool_results(&requests[2].json());
    let verdict = results
        .iter()
        .find(|r| r.get("risk_level").is_some())
        .unwrap_or_else(|| panic!("no chi-square result in {results:?}"));
    assert_eq!(verdict["risk_level"], "LOW", "{verdict}");
    assert_eq!(verdict["degrees_of_freedom"], 8, "{verdict}");
    assert_eq!(verdict["most_deviant_digit"], 1, "{verdict}");
    let chi_square = verdict["chi_square"].as_f64().expect("a statistic");
    assert!(
        (chi_square - 10.49).abs() < 0.01,
        "chi-square {chi_square}, expected ≈ 10.49"
    );
    assert!(stdout.contains("LOW"), "{stdout}");
}
