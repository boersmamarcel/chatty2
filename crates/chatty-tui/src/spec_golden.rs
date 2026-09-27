//! Goldens for the `coder-reviewer` team (AGE-614): what its leader and
//! workers are built with, and the leader's prompt-cache prefix.
//!
//! The goldens were recorded from `main` before agents became specs, so a
//! rewrite of how a team is declared has to reproduce them exactly.
//! `UPDATE_GOLDENS=1` rewrites them.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chatty_core::factories::agent_factory::{
    AgentBuildContext, AgentClient, AgentServices, gated_exec_settings,
};
use chatty_core::models::clarification_store::ClarificationStore;
use chatty_core::models::execution_approval_store::ExecutionApprovalStore;
use chatty_core::models::write_approval_store::WriteApprovalStore;
use chatty_core::services::team::load_team;
use chatty_core::services::virtual_agents::resolve_virtual_agents;
use chatty_core::settings::models::execution_settings::ApprovalMode;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::settings::models::{ExecutionSettingsModel, ModuleSettingsModel};
use clap::Parser;
use rig_agent::completion::Prompt;
use serde_json::{Value, json};

use crate::{Cli, apply_tool_only, apply_tool_overrides, resolve_role, unattended_run_limits};

/// The workspace every context is built in; only its name reaches a golden.
const WORKSPACE: &str = "/golden/workspace";

fn golden_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/goldens")
        .join(name)
}

fn check_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{} is missing ({e}); run with UPDATE_GOLDENS=1",
            path.display()
        )
    });
    assert_eq!(
        actual,
        expected,
        "{} changed; if that is intended, rerun with UPDATE_GOLDENS=1",
        path.display()
    );
}

fn base_settings(workspace: &str) -> ExecutionSettingsModel {
    ExecutionSettingsModel {
        workspace_dir: Some(workspace.to_string()),
        ..ExecutionSettingsModel::default()
    }
}

/// The part of a context an agent spec decides, as JSON.
fn project(ctx: &AgentBuildContext, model: Option<&str>) -> Value {
    json!({
        "model": model,
        "role": {
            "preamble": ctx.role.preamble,
            "profile": ctx.role.profile.map(|p| p.name()),
        },
        "exec_settings": ctx.exec_settings,
        "ask_user_enabled": ctx.ask_user_enabled,
        "instructions_dir": ctx.instructions_dir,
        "local_agents": ctx.local_agents,
        "team_skill": ctx.team_skill.as_ref().map(|s| json!({"name": s.name, "content": s.content})),
        "unattended": ctx.unattended,
        "spend_gate": ctx.spend_gate.is_some(),
    })
}

/// The `coder-reviewer` leader as `--team coder-reviewer --headless` builds
/// it: its context and the model it asks for.
fn leader_context(workspace: &str) -> (AgentBuildContext, Option<String>) {
    let team = load_team("coder-reviewer", None, None).expect("the preset loads");
    let mut settings = base_settings(workspace);
    team.apply_turn_budget(&mut settings);
    let (turns, _) = unattended_run_limits(None, team.file.max_agent_turns, None);
    settings.max_agent_turns = turns;
    let role = resolve_role(
        team.file.leader.profile.as_deref(),
        team.file.leader.preamble.as_deref(),
    )
    .expect("the leader's role resolves");
    let ctx = AgentBuildContext {
        role,
        team_skill: team.skill(),
        unattended: true,
        ask_user_enabled: settings.ask_user_enabled,
        instructions_dir: Some(PathBuf::from(workspace)),
        ..AgentBuildContext::from_services(AgentServices {
            exec_settings: gated_exec_settings(&settings),
            local_agents: team.agent_names(),
            ..AgentServices::default()
        })
    };
    (ctx, team.file.leader.model.clone())
}

/// Each `coder-reviewer` worker as its `chatty-tui` child builds itself from
/// the argv the broker spawns it with.
fn worker_contexts(workspace: &str) -> Vec<(String, AgentBuildContext, Option<String>)> {
    let team = load_team("coder-reviewer", None, None).expect("the preset loads");
    let module_settings = team.run_module_settings(&ModuleSettingsModel::default());
    resolve_virtual_agents(&[], &[], &module_settings, &["--auto-approve".to_string()])
        .into_iter()
        .map(|spec| {
            let mut argv = vec!["chatty-tui".to_string()];
            argv.extend(spec.args.iter().cloned());
            argv.extend(
                [
                    "--participant-socket",
                    "/golden/sock",
                    "--participant-name",
                    "w-0",
                ]
                .map(str::to_string),
            );
            let cli = Cli::try_parse_from(&argv).expect("the worker's argv parses");
            let mut settings = base_settings(workspace);
            let (turns, _) = unattended_run_limits(cli.max_agent_turns, None, cli.max_duration);
            settings.max_agent_turns = turns;
            apply_tool_overrides(&mut settings, &cli.enable, &cli.disable).unwrap();
            if !cli.only.is_empty() {
                apply_tool_only(&mut settings, &cli.only).unwrap();
            }
            let role = resolve_role(cli.tools.as_deref(), cli.preamble.as_deref()).unwrap();
            if cli.auto_approve {
                settings.approval_mode = ApprovalMode::AutoApproveAll;
            }
            let ctx = AgentBuildContext {
                role,
                unattended: true,
                ask_user_enabled: settings.ask_user_enabled,
                instructions_dir: Some(PathBuf::from(workspace)),
                ..AgentBuildContext::from_services(AgentServices {
                    exec_settings: gated_exec_settings(&settings),
                    local_agents: ModuleSettingsModel::default().virtual_agent_names(),
                    ..AgentServices::default()
                })
            };
            (spec.name, ctx, cli.model)
        })
        .collect()
}

#[test]
fn coder_reviewer_preset_builds_same_context() {
    let (leader, leader_model) = leader_context(WORKSPACE);
    let mut roles = serde_json::Map::new();
    roles.insert(
        "leader".to_string(),
        project(&leader, leader_model.as_deref()),
    );
    for (name, ctx, model) in worker_contexts(WORKSPACE) {
        roles.insert(name, project(&ctx, model.as_deref()));
    }
    let actual = serde_json::to_string_pretty(&Value::Object(roles)).unwrap() + "\n";
    check_golden("coder_reviewer_context.json", &actual);
}

/// A model server that records every request body and answers none.
async fn recording_server() -> (String, Arc<Mutex<Vec<Value>>>) {
    let bodies: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = bodies.clone();
    let app = axum::Router::new().fallback(move |body: String| {
        let sink = sink.clone();
        async move {
            if let Ok(json) = serde_json::from_str::<Value>(&body) {
                sink.lock().unwrap().push(json);
            }
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "no model here",
            )
        }
    });
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(tcp, app).await.ok();
    });
    (format!("http://127.0.0.1:{port}"), bodies)
}

/// The leader's system message and tool schema are the prompt-cache prefix:
/// every turn of every run of the team starts with these bytes.
#[tokio::test]
async fn leader_prefix_is_byte_identical() {
    let _ = chatty_core::init_repositories();
    let workspace = tempfile::tempdir().expect("a workspace");
    let workspace_str = workspace.path().to_string_lossy().into_owned();
    let (ctx, _) = leader_context(&workspace_str);
    let ctx = AgentBuildContext {
        gateway_port: Some(1),
        pending_approvals: Some(ExecutionApprovalStore::new().get_pending_approvals()),
        pending_clarifications: Some(ClarificationStore::new().get_pending_clarifications()),
        pending_write_approvals: Some(WriteApprovalStore::new().get_pending_approvals()),
        ..ctx
    };
    let (base_url, bodies) = recording_server().await;
    let mut provider = ProviderConfig::new("Ollama".to_string(), ProviderType::Ollama);
    provider.base_url = Some(base_url);
    let built = AgentClient::from_model_config_with_tools(
        &ModelConfig::new(
            "golden-model".to_string(),
            "golden-model".to_string(),
            ProviderType::Ollama,
            "golden-model".to_string(),
        ),
        &provider,
        ctx,
    )
    .await
    .expect("the leader builds without network access");
    let _ = built.client.agent.prompt("go").await;
    let body = bodies
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("one request");
    let prefix = json!({
        "system": body["messages"][0]["content"],
        "tools": body["tools"],
    });
    let actual = serde_json::to_string_pretty(&prefix)
        .unwrap()
        .replace(&workspace_str, WORKSPACE)
        + "\n";
    check_golden("coder_reviewer_leader_prefix.json", &actual);
}
