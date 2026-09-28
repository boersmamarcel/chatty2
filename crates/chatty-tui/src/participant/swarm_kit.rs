//! The swarm test kit (AGE-632): a real root broker, real `chatty-tui`
//! worker processes, and a scripted fake model behind all of them.
//!
//! `equivalence.rs` scripts a worker's `SessionEvent`s in-process; nothing
//! there runs a worker binary. A swarm feature spans processes — the
//! leader's broker, a worker child, a sub-leader's own broker and its
//! grandchild — so its tests run here instead:
//!
//! - [`SwarmKit::start`] writes a throwaway config (providers, models,
//!   execution and module settings) whose models point at two
//!   [`FakeDaemon`]s — one OpenAI-compatible (SSE) endpoint, one Ollama
//!   (NDJSON) endpoint, so "separate endpoints" is two providers — and starts
//!   a root [`Broker`] with the declared roster.
//! - Every worker is the real `chatty-tui` from
//!   [`worker_executable`], started through a two-line `sh` wrapper that only
//!   points `HOME` and the XDG directories at the kit's temp dir, so a worker
//!   reads the kit's config and never the developer's.
//! - [`SwarmKit::run_leader`] is the leader's `invoke_agent` call: it returns
//!   what the tool handed the model, the progress the parent's transcript
//!   renders, and the fake models' request logs.
//!
//! Each agent's model identifier is its routing key on the fake server, so
//! a [`Script`] route per agent answers that agent's requests and nothing
//! else. Nothing leaves loopback: a worker reaching the internet fails the
//! test in a sandbox that forbids it, and a request no route matches gets a
//! 500 naming its model.
//!
//! # Pre-change goldens (invariant 10)
//!
//! `goldens/pre_fabric/` holds the parent-side trace of every
//! `equivalence.rs` scenario the kit can drive with a real worker, and of
//! nested delegation on separate endpoints, recorded on `main` before BI-3.
//! [`pre_fabric_goldens_replay`] replays them. `UPDATE_GOLDENS=1` does not
//! touch them: [`assert_pre_fabric`] only ever writes a golden that does not
//! exist yet, and only under `RECORD_PRE_FABRIC_GOLDENS=1`. A later PR may
//! delete one with a stated reason; it may not re-record one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chatty_core::agent_spec::AgentSpec;
use chatty_core::models::token_usage::TokenUsage;
use chatty_core::services::install_progress_channel;
use chatty_core::services::virtual_agents::resolve_virtual_agents;
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::settings::models::execution_settings::{ApprovalMode, ExecutionSettingsModel};
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::{ProviderConfig, ProviderType};
use chatty_core::testing::fake_model::{FakeDaemon, RecordedRequest, Reply, Script};
use chatty_core::tools::invoke_agent_tool::{
    InvokeAgentArgs, InvokeAgentOutput, InvokeAgentProgress, InvokeAgentTool,
};
use chatty_core::tools::worker_executable;
use chatty_fabric::HandoffContract;
use rig_agent::tool::{Tool, ToolContext};

use super::broker::Broker;

/// Which fake model endpoint an agent's model is served from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Endpoint {
    /// OpenAI-compatible SSE, behind an OpenRouter-type provider.
    Sse,
    /// Ollama NDJSON, behind an Ollama provider.
    Ndjson,
}

/// One roster entry: a virtual agent and the model it runs on.
#[derive(Debug, Clone)]
pub(crate) struct AgentDef {
    pub name: String,
    /// The model identifier, which is also this agent's routing key on the
    /// fake server.
    pub model: String,
    pub endpoint: Endpoint,
    /// Declare `swarm.delegates_to`: a sub-leader, which delegates in turn
    /// over its connection to the root broker (BI-5).
    pub sub_leader: bool,
    /// The spec it runs as, with `model` laid over it; a bare one when
    /// `None`.
    pub spec: Option<AgentSpec>,
    /// The spec's `[tools] profile`, e.g. `coder`, over `spec`'s.
    pub profile: Option<&'static str>,
    /// The JSON Schema the team names for this role's handoff (TD-2).
    pub handoff: Option<serde_json::Value>,
}

impl AgentDef {
    pub fn new(name: &str, model: &str, endpoint: Endpoint) -> Self {
        Self {
            name: name.to_string(),
            model: model.to_string(),
            endpoint,
            sub_leader: false,
            spec: None,
            profile: None,
            handoff: None,
        }
    }

    /// An agent that runs as `spec` (a preset, say) on `model`.
    pub fn from_spec(spec: AgentSpec, model: &str, endpoint: Endpoint) -> Self {
        Self {
            spec: Some(spec.clone()),
            ..Self::new(&spec.agent.name, model, endpoint)
        }
    }

    /// Name a handoff schema for this role, as a team's `handoffs` does.
    pub fn with_handoff(mut self, schema: serde_json::Value) -> Self {
        self.handoff = Some(schema);
        self
    }

    pub fn sub_leader(mut self) -> Self {
        self.sub_leader = true;
        self
    }

    pub fn profile(mut self, profile: &'static str) -> Self {
        self.profile = Some(profile);
        self
    }
}

/// What the leader's `invoke_agent` call came back with.
#[derive(Debug)]
pub(crate) struct LeaderRun {
    /// The tool's result; the error is rendered as the model would see it.
    pub output: Result<InvokeAgentOutput, String>,
    /// Every progress event, in order.
    pub progress: Vec<InvokeAgentProgress>,
    /// The SSE endpoint's requests, then the NDJSON endpoint's.
    pub requests: Vec<RecordedRequest>,
}

/// A running swarm: two fake endpoints, a root broker, a throwaway home.
pub(crate) struct SwarmKit {
    pub sse: FakeDaemon,
    pub ndjson: FakeDaemon,
    roster: Vec<String>,
    broker: Option<Broker>,
    root: tempfile::TempDir,
}

/// How long one delegation may take before the test fails instead of hangs.
const DEADLINE: Duration = Duration::from_secs(60);

impl SwarmKit {
    /// Start a root broker serving `roster`, the SSE endpoint answering from
    /// `sse` and the NDJSON one from `ndjson`. The workspace holds one file,
    /// `README.md` (`# Chatty`).
    pub async fn start(roster: Vec<AgentDef>, sse: Script, ndjson: Script) -> Self {
        Self::start_with(roster, sse, ndjson, false).await
    }

    /// As [`start`](Self::start), with the workspace a git repository on
    /// `main` whose one commit holds `README.md`, so every worker gets a
    /// `git worktree` of its own (BI-5).
    pub async fn start_in_repo(roster: Vec<AgentDef>, sse: Script, ndjson: Script) -> Self {
        Self::start_with(roster, sse, ndjson, true).await
    }

    async fn start_with(roster: Vec<AgentDef>, sse: Script, ndjson: Script, repo: bool) -> Self {
        let root = tempfile::tempdir().expect("a temp dir for the swarm");
        let base = root.path().canonicalize().expect("the temp dir resolves");
        let sse = FakeDaemon::scripted(sse);
        let ndjson = FakeDaemon::scripted(ndjson);

        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::write(workspace.join("README.md"), "# Chatty\n").expect("README.md");
        if repo {
            for args in [
                &["init", "-q", "-b", "main"][..],
                &["config", "user.email", "kit@example.com"],
                &["config", "user.name", "Kit"],
                &["add", "README.md"],
                &["commit", "-q", "-m", "init"],
            ] {
                git(&workspace, args);
            }
        }

        let providers = vec![
            ProviderConfig::new("Fake SSE".to_string(), ProviderType::OpenRouter)
                .with_api_key("swarm-kit-key".to_string())
                .with_base_url(sse.base_url()),
            ProviderConfig::new("Fake NDJSON".to_string(), ProviderType::Ollama)
                .with_base_url(ndjson.base_url()),
        ];
        let models: Vec<ModelConfig> = roster
            .iter()
            .map(|agent| {
                let provider = match agent.endpoint {
                    Endpoint::Sse => ProviderType::OpenRouter,
                    Endpoint::Ndjson => ProviderType::Ollama,
                };
                ModelConfig::new(
                    agent.model.clone(),
                    agent.model.clone(),
                    provider,
                    agent.model.clone(),
                )
            })
            .collect();
        // Each agent is a spec in the kit's data directory, where a
        // sub-leader's own broker finds the roster module settings name.
        let specs: Vec<AgentSpec> = roster
            .iter()
            .map(|agent| {
                let mut spec = agent
                    .spec
                    .clone()
                    .unwrap_or_else(|| AgentSpec::named(&agent.name));
                spec.agent.model = Some(agent.model.clone());
                if agent.sub_leader {
                    spec.swarm.delegates_to = vec!["*".to_string()];
                }
                if let Some(profile) = agent.profile {
                    spec.tools.profile = Some(profile.to_string());
                }
                spec
            })
            .collect();
        let agents_dir = base.join("data").join("chatty").join("agents");
        std::fs::create_dir_all(&agents_dir).expect("agents dir");
        for spec in &specs {
            std::fs::write(
                agents_dir.join(format!("{}.toml", spec.agent.name)),
                spec.to_toml().expect("a spec serializes"),
            )
            .expect("spec file");
        }
        let module_settings = ModuleSettingsModel {
            virtual_agents: roster.iter().map(|agent| agent.name.clone()).collect(),
            ..ModuleSettingsModel::default()
        };
        let execution = ExecutionSettingsModel {
            enabled: true,
            workspace_dir: Some(workspace.to_string_lossy().into_owned()),
            fetch_enabled: false,
            memory_enabled: false,
            approval_mode: ApprovalMode::AutoApproveAll,
            // A worker commits with the git tools: its sandboxed shell
            // cannot reach a linked worktree's repository.
            git_enabled: repo,
            ..ExecutionSettingsModel::default()
        };

        let config = base.join("config").join("chatty");
        std::fs::create_dir_all(&config).expect("config dir");
        for (file, value) in [
            ("providers.json", serde_json::to_value(&providers)),
            ("models.json", serde_json::to_value(&models)),
            ("execution_settings.json", serde_json::to_value(&execution)),
            (
                "module_settings.json",
                serde_json::to_value(&module_settings),
            ),
        ] {
            let json = serde_json::to_string_pretty(&value.expect("settings serialize"))
                .expect("settings serialize");
            std::fs::write(config.join(file), json).expect("settings file");
        }

        let executable = wrapper(&base, &worker_executable());
        let mut specs = resolve_virtual_agents(
            &models,
            &providers,
            &module_settings,
            &specs,
            &["--auto-approve".to_string()],
        );
        // What `Broker::start` does with a team's `handoffs` (TD-2).
        for (spec, agent) in specs.iter_mut().zip(&roster) {
            spec.handoff = agent.handoff.clone().map(|schema| HandoffContract {
                role: agent.name.clone(),
                schema,
            });
        }
        let broker = Broker::start_at(
            base.join("run").join("participants.sock"),
            executable,
            module_settings.default_endpoint_budget,
            specs,
            Some(workspace.to_string_lossy().into_owned()),
        )
        .await
        .expect("the root broker starts");

        Self {
            sse,
            ndjson,
            roster: roster.into_iter().map(|a| a.name).collect(),
            broker: Some(broker),
            root,
        }
    }

    /// The workspace every worker's tree is made under.
    pub fn workspace(&self) -> PathBuf {
        self.root().join("workspace")
    }

    /// The temp dir everything lives under, as the workers see it.
    pub fn root(&self) -> PathBuf {
        self.root
            .path()
            .canonicalize()
            .expect("the temp dir resolves")
    }

    /// The root broker's shared participant socket, which refuses every
    /// registration (ADR-0020).
    pub fn socket(&self) -> PathBuf {
        self.root().join("run").join("participants.sock")
    }

    /// Who is connected to the root broker.
    pub fn participants(&self) -> chatty_protocol_gateway::participant::ParticipantRegistry {
        self.broker
            .as_ref()
            .expect("the broker is running")
            .participants()
    }

    /// The leader delegates `prompt` to the first agent in the roster.
    pub async fn run_leader(&self, prompt: &str) -> LeaderRun {
        let agent = self
            .roster
            .first()
            .expect("the roster is not empty")
            .clone();
        self.run_leader_to(&agent, prompt).await
    }

    /// The leader delegates `prompt` to `agent` through the real
    /// `invoke_agent` tool, over the root broker's direct handle — the
    /// in-process root reaches its broker without a socket or an HTTP hop
    /// (ADR-0020, BI-4).
    pub async fn run_leader_to(&self, agent: &str, prompt: &str) -> LeaderRun {
        self.run_leader_with(self.leader_tool(), agent, prompt)
            .await
    }

    /// As [`run_leader_to`](Self::run_leader_to), through `tool`: the
    /// leader's `invoke_agent` with something of the test's own added.
    pub async fn run_leader_with(
        &self,
        tool: InvokeAgentTool,
        agent: &str,
        prompt: &str,
    ) -> LeaderRun {
        let mut progress_rx = install_progress_channel(&tool.progress_slot());

        let output = tokio::time::timeout(
            DEADLINE,
            tool.call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: agent.to_string(),
                    prompt: prompt.to_string(),
                    include_trace: false,
                },
            ),
        )
        .await
        .expect("the delegation finishes before the deadline")
        .map_err(|e| e.to_string());

        let mut progress = Vec::new();
        while let Ok(event) = progress_rx.try_recv() {
            progress.push(event);
        }
        let mut requests = self.sse.requests();
        requests.extend(self.ndjson.requests());
        LeaderRun {
            output,
            progress,
            requests,
        }
    }
}

impl SwarmKit {
    /// The leader's `invoke_agent`, holding the root broker's direct handle.
    pub fn leader_tool(&self) -> InvokeAgentTool {
        InvokeAgentTool::new(vec![])
            .with_local_agents(self.roster.clone())
            .with_transport(self.broker().transport())
    }

    /// The root broker.
    pub fn broker(&self) -> &Broker {
        self.broker.as_ref().expect("the broker is running")
    }
}

impl Drop for SwarmKit {
    fn drop(&mut self) {
        if let Some(broker) = self.broker.take() {
            broker.shutdown();
        }
    }
}

/// `git <args>` in `dir`, which must succeed; its trimmed stdout.
pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?} in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A `chatty-tui` that runs `real` with the kit's home and XDG dirs, so the
/// worker reads the kit's settings. Everything else is inherited.
fn wrapper(base: &Path, real: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    assert!(
        real.is_absolute() && real.exists(),
        "no chatty-tui next to the test binary ({}); `cargo test -p chatty-tui` builds it",
        real.display()
    );
    let dir = |name: &str| {
        let path = base.join(name);
        std::fs::create_dir_all(&path).expect("xdg dir");
        path.to_string_lossy().into_owned()
    };
    let script = format!(
        "#!/bin/sh\nexport HOME='{}' XDG_CONFIG_HOME='{}' XDG_DATA_HOME='{}' \
         XDG_CACHE_HOME='{}' XDG_STATE_HOME='{}' XDG_RUNTIME_DIR='{}'\nexec '{}' \"$@\"\n",
        dir("home"),
        dir("config"),
        dir("data"),
        dir("cache"),
        dir("state"),
        dir("run"),
        real.display()
    );
    let path = base.join("chatty-tui");
    std::fs::write(&path, script).expect("worker wrapper");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("worker wrapper is executable");
    path
}

// ---------------------------------------------------------------------------
// Normalising: what differs run to run and is not the behaviour under test
// ---------------------------------------------------------------------------

/// `text` with the kit's temp dir, the fake servers' ports and tool-call ids
/// replaced by placeholders.
pub(crate) fn normalize(kit: &SwarmKit, text: &str) -> String {
    let mut text = text
        .replace(&kit.root().to_string_lossy().to_string(), "<ROOT>")
        .replace(&kit.sse.base_url(), "<SSE>")
        .replace(&kit.ndjson.base_url(), "<NDJSON>");
    // Worker ids and task ids are uuids; a stable placeholder keeps the
    // shape visible in a diff.
    text = replace_uuids(&text);
    text
}

/// Replace every 8-4-4-4-12 hex uuid with `<ID>`.
fn replace_uuids(text: &str) -> String {
    let bytes = text.as_bytes();
    let shape = [8usize, 4, 4, 4, 12];
    let len = 36;
    let is_uuid = |at: usize| {
        if at + len > bytes.len() {
            return false;
        }
        let mut i = at;
        for (n, run) in shape.iter().enumerate() {
            if !bytes[i..i + run].iter().all(u8::is_ascii_hexdigit) {
                return false;
            }
            i += run;
            if n < shape.len() - 1 {
                if bytes[i] != b'-' {
                    return false;
                }
                i += 1;
            }
        }
        true
    };
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < bytes.len() {
        if is_uuid(at) {
            out.push_str("<ID>");
            at += len;
        } else {
            let ch = text[at..].chars().next().expect("a char at a boundary");
            out.push(ch);
            at += ch.len_utf8();
        }
    }
    out
}

/// The parent-side trace of a run, one line per event: what the golden
/// records. Usage is the token counts only, so a usage line gaining fields
/// (a model name, a time) does not read as a behaviour change.
pub(crate) fn parent_trace(kit: &SwarmKit, run: &LeaderRun) -> Vec<String> {
    let mut lines = Vec::new();
    for event in &run.progress {
        lines.push(match event {
            InvokeAgentProgress::Started {
                agent_name, prompt, ..
            } => format!("started {agent_name}: {prompt}"),
            // A step and an answer chunk read alike in the goldens, which
            // were recorded before the two were told apart (BI-4).
            InvokeAgentProgress::Text(text) | InvokeAgentProgress::Step(text) => {
                format!("progress {text}")
            }
            InvokeAgentProgress::Finished {
                success,
                result,
                usage,
            } => {
                // The lines' token totals, so one line per model (AGE-682)
                // reads exactly as the one usage value the goldens recorded.
                // The wire carries no call count, so each decoded line's is
                // the default 1: summing it would count lines, not calls.
                let usage = if usage.is_empty() {
                    "none".to_string()
                } else {
                    let sum = |f: fn(&TokenUsage) -> u32| usage.iter().map(f).sum::<u32>();
                    format!(
                        "input={} output={} cache_read={} cache_write={} calls={}",
                        sum(|u| u.input_tokens),
                        sum(|u| u.output_tokens),
                        sum(|u| u.cache_read_tokens),
                        sum(|u| u.cache_write_tokens),
                        usage.iter().map(|u| u.api_turn_count).max().unwrap_or(0)
                    )
                };
                format!(
                    "finished success={success} usage=[{usage}] result={}",
                    result.as_deref().unwrap_or("-")
                )
            }
        });
    }
    lines.push(match &run.output {
        Ok(out) => format!("answer success={}: {}", out.success, out.response),
        Err(error) => format!("error: {error}"),
    });
    lines
        .into_iter()
        .map(|line| normalize(kit, &line).replace('\n', "\\n"))
        .collect()
}

/// The pre-change goldens' directory.
fn pre_fabric_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/participant/goldens/pre_fabric")
}

/// Compare `lines` with the committed golden `name`. A missing golden is
/// written only under `RECORD_PRE_FABRIC_GOLDENS=1`; an existing one is
/// never rewritten, whatever the environment says (invariant 10).
pub(crate) fn assert_pre_fabric(name: &str, lines: &[String]) {
    let path = pre_fabric_dir().join(format!("{name}.txt"));
    let recorded = format!("{}\n", lines.join("\n"));
    let Ok(expected) = std::fs::read_to_string(&path) else {
        if std::env::var("RECORD_PRE_FABRIC_GOLDENS").is_ok() {
            std::fs::create_dir_all(pre_fabric_dir()).expect("golden dir");
            std::fs::write(&path, &recorded).expect("golden written");
            return;
        }
        panic!(
            "missing pre-fabric golden {}. These were recorded once, before BI-3, \
             and are never re-recorded.",
            path.display()
        );
    };
    assert_eq!(
        expected,
        recorded,
        "the parent-side trace of '{name}' differs from the pre-fabric golden {}",
        path.display()
    );
}

// ---------------------------------------------------------------------------
// Scenarios: equivalence.rs's, driven by a real worker
// ---------------------------------------------------------------------------

/// The one worker most scenarios need, on the SSE endpoint.
const WORKER: &str = "kit-worker";
const WORKER_MODEL: &str = "kit/worker";

/// A named scenario: the roster, the two endpoints' scripts, the prompt.
pub(crate) struct KitScenario {
    pub name: &'static str,
    pub roster: Vec<AgentDef>,
    pub sse: Script,
    pub ndjson: Script,
    pub prompt: &'static str,
}

fn one_worker(name: &'static str, replies: Vec<Reply>) -> KitScenario {
    KitScenario {
        name,
        roster: vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        sse: Script::new().route(WORKER_MODEL, replies),
        ndjson: Script::new(),
        prompt: "do the delegated task",
    }
}

/// `equivalence.rs`'s scenarios (`stream_fixtures::scenarios()` plus the
/// clarification one), as a real worker can produce them, and nested
/// delegation on separate endpoints.
///
/// Left out, because a real worker cannot be scripted into them from the
/// model's side: `approval_granted`/`approval_denied` (a worker runs
/// auto-approved, so no approval is ever raised), `cancelled_mid_stream`
/// (Stop is a human's, and a delegated worker has none) and
/// `delegation_progress`, which is `nested_delegation_separate_endpoints`
/// here with a real sub-leader instead of canned progress. Nested
/// delegation on one budget-1 endpoint is excluded by invariant 10: it is
/// expected to change at BI-6.
pub(crate) fn kit_scenarios() -> Vec<KitScenario> {
    vec![
        one_worker("text_only", vec![Reply::text("Hello, world")]),
        one_worker(
            "tool_call_then_result",
            vec![
                Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" })),
                Reply::text("It is the readme."),
            ],
        ),
        one_worker(
            "tool_error",
            vec![
                Reply::tool_call("read_file", serde_json::json!({ "path": "missing.md" })),
                Reply::text("That read failed."),
            ],
        ),
        one_worker(
            "token_usage_on_done",
            vec![
                Reply::Usage {
                    input: 234,
                    output: 56,
                    cache_read: 1000,
                },
                Reply::text("Answer."),
            ],
        ),
        // `equivalence.rs`'s `provider_error_mid_stream`, as an HTTP error: a
        // headless worker retries any other status after 10 s, 20 s, …, and a
        // 401 once at once, so two 401s end the worker's turn quickly.
        one_worker("provider_error", vec![Reply::Error(401), Reply::Error(401)]),
        one_worker(
            "tool_results_without_plan",
            vec![
                Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" })),
                Reply::tool_call("list_directory", serde_json::json!({ "path": "." })),
                Reply::text("Still investigating."),
            ],
        ),
        one_worker(
            "clarification_requested",
            vec![Reply::tool_call(
                "ask_user",
                serde_json::json!({ "questions": [{
                    "id": "q1",
                    "question": "Deploy to production?",
                    "options": ["Yes", "No"]
                }] }),
            )],
        ),
        KitScenario {
            name: "nested_delegation_separate_endpoints",
            roster: vec![
                AgentDef::new("kit-lead", "kit/lead", Endpoint::Sse).sub_leader(),
                AgentDef::new("kit-helper", "kit/helper", Endpoint::Ndjson),
            ],
            sse: Script::new().route(
                "kit/lead",
                [
                    Reply::tool_call(
                        "invoke_agent",
                        serde_json::json!({ "agent": "kit-helper", "prompt": "say hello" }),
                    ),
                    Reply::text("The helper said hello."),
                ],
            ),
            ndjson: Script::new().route("kit/helper", [Reply::text("hello")]),
            prompt: "ask the helper to say hello",
        },
    ]
}

async fn run_scenario(scenario: KitScenario) -> (SwarmKit, LeaderRun) {
    let kit = SwarmKit::start(scenario.roster, scenario.sse, scenario.ndjson).await;
    let run = kit.run_leader(scenario.prompt).await;
    (kit, run)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A leader delegates to a real child process; the child makes one scripted
/// tool call and answers; the leader receives the answer.
#[tokio::test]
async fn swarm_kit_two_process_delegation() {
    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" })),
                Reply::text("The readme says Chatty."),
            ],
        ),
        Script::new(),
    )
    .await;

    let run = kit.run_leader("read the readme").await;

    let out = run.output.as_ref().expect("the delegation succeeded");
    assert!(out.success);
    assert_eq!(out.response, "The readme says Chatty.");
    let requests = kit.sse.requests_for(WORKER_MODEL);
    assert_eq!(
        requests.len(),
        2,
        "one call for the tool, one for the answer"
    );
    assert!(
        String::from_utf8_lossy(&requests[1].body).contains("# Chatty"),
        "the child really ran read_file and sent its result back to the model"
    );
    let lines: Vec<_> = run
        .progress
        .iter()
        .filter_map(|p| match p {
            InvokeAgentProgress::Text(text) | InvokeAgentProgress::Step(text) => {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(
        lines.contains(&"read_file") && lines.contains(&"\u{2713} read_file"),
        "the parent saw the child's tool call: {lines:?}"
    );
}

/// The same script run 20 times gives identical request bodies once the
/// temp dir, ports and ids are normalised.
#[tokio::test]
async fn swarm_kit_is_deterministic() {
    let runs = futures::future::join_all((0..20).map(|_| async {
        let scenario = kit_scenarios()
            .into_iter()
            .find(|s| s.name == "tool_call_then_result")
            .expect("the scenario exists");
        let (kit, run) = run_scenario(scenario).await;
        assert!(run.output.is_ok(), "{:?}", run.output);
        run.requests
            .iter()
            .map(|r| normalize(&kit, &String::from_utf8_lossy(&r.body)))
            .collect::<Vec<_>>()
    }))
    .await;

    assert_eq!(runs[0].len(), 2);
    for (index, bodies) in runs.iter().enumerate().skip(1) {
        assert_eq!(
            &runs[0], bodies,
            "run {index} sent different request bodies"
        );
    }
}

/// Invariant 10: the parent-side traces recorded before BI-3 still hold.
#[tokio::test]
async fn pre_fabric_goldens_replay() {
    let runs = futures::future::join_all(kit_scenarios().into_iter().map(|scenario| async {
        let name = scenario.name;
        let (kit, run) = run_scenario(scenario).await;
        (name, parent_trace(&kit, &run))
    }))
    .await;

    for (name, lines) in runs {
        assert_pre_fabric(name, &lines);
    }
}

// ---------------------------------------------------------------------------
// Broker-made connections (BI-3, AGE-635; ADR-0020 invariants 1 and 2)
// ---------------------------------------------------------------------------

/// Invariant 1: a process that did not receive a broker-made connection
/// cannot register as any node. A same-user process dials the shared socket
/// and claims the name the broker is about to give its first worker, the v1
/// way and the v2 way; both are refused and closed, and the real worker —
/// spawned on the connection the broker made — still gets its task.
#[tokio::test]
async fn squatting_on_the_shared_socket_is_refused() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(WORKER_MODEL, [Reply::text("The real worker answered.")]),
        Script::new(),
    )
    .await;
    let first_worker = format!("{WORKER}-0");

    for claim in [
        serde_json::json!({ "type": "register", "card": { "name": first_worker } }),
        serde_json::json!({ "v": 2, "type": "hello", "card": { "name": first_worker } }),
    ] {
        let stream = tokio::net::UnixStream::connect(kit.socket())
            .await
            .expect("the shared socket accepts a connection");
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        write
            .write_all(format!("{claim}\n").as_bytes())
            .await
            .unwrap();
        let reply: serde_json::Value = serde_json::from_str(
            &lines
                .next_line()
                .await
                .unwrap()
                .expect("the squatter is answered"),
        )
        .unwrap();
        assert_eq!(reply["type"], "error", "{claim} is refused: {reply}");
        assert!(
            lines.next_line().await.unwrap().is_none(),
            "and its connection closed"
        );
        assert!(!kit.participants().is_registered(&first_worker));
    }

    let run = kit.run_leader("do the delegated task").await;
    let out = run.output.as_ref().expect("the delegation succeeded");
    assert!(out.success);
    assert_eq!(out.response, "The real worker answered.");
    assert_eq!(
        kit.sse.requests_for(WORKER_MODEL).len(),
        1,
        "the real worker made the model call"
    );
}

/// The `socket:[inode]` link of the broker-made connection held by the
/// worker of `kit`: its `chatty-tui` child whose `HOME` is the kit's, read
/// at the descriptor its `--participant-fd` names. Polls until the worker
/// is up.
async fn worker_connection_link(kit: &SwarmKit) -> String {
    let parent = std::process::id().to_string();
    let home = format!("HOME={}", kit.root().join("home").display());
    let deadline = std::time::Instant::now() + DEADLINE;
    while std::time::Instant::now() < deadline {
        for entry in std::fs::read_dir("/proc").expect("/proc is readable") {
            let proc_dir = entry.expect("a /proc entry").path();
            let is_child = std::fs::read_to_string(proc_dir.join("status"))
                .is_ok_and(|status| status.lines().any(|l| l == format!("PPid:\t{parent}")));
            let is_ours = std::fs::read(proc_dir.join("environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|var| var == home.as_bytes()));
            if !(is_child && is_ours) {
                continue;
            }
            let argv = std::fs::read(proc_dir.join("cmdline")).unwrap_or_default();
            let argv: Vec<String> = argv
                .split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            let Some(fd) = argv
                .iter()
                .position(|a| a == "--participant-fd")
                .and_then(|at| argv.get(at + 1))
            else {
                continue;
            };
            if let Ok(link) = std::fs::read_link(proc_dir.join("fd").join(fd)) {
                return link.to_string_lossy().into_owned();
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("no worker of this kit came up holding a participant descriptor");
}

/// Invariant 2: neither the worker's persistent shell nor a process that
/// shell starts has the participant socket open. The model has the shell
/// list its own descriptors (`/proc/$$/fd`: `$$` is the shell) and a
/// child's (`/proc/self/fd` inside `sh -c` is that `ls`'s own); the socket's
/// inode — read from the worker's own descriptor table, where it must be —
/// is in neither.
#[tokio::test]
async fn worker_shell_cannot_see_the_participant_fd() {
    const LISTING: &str = "ls -l /proc/$$/fd; sh -c 'ls -l /proc/self/fd'";
    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::tool_call("shell_execute", serde_json::json!({ "command": LISTING })),
                // Keeps the worker up while its descriptor table is read.
                Reply::Delay(300),
                Reply::text("Listed."),
            ],
        ),
        Script::new(),
    )
    .await;

    let (link, run) = tokio::join!(
        worker_connection_link(&kit),
        kit.run_leader("list your descriptors")
    );
    let out = run.output.as_ref().expect("the delegation succeeded");
    assert_eq!(out.response, "Listed.");
    assert!(
        link.starts_with("socket:["),
        "the worker holds its connection at --participant-fd: {link}"
    );

    let requests = kit.sse.requests_for(WORKER_MODEL);
    let listing = String::from_utf8_lossy(&requests[1].body);
    assert!(
        listing.matches(" -> ").count() >= 6,
        "the shell really listed both descriptor tables: {listing}"
    );
    assert!(
        !listing.contains(&link),
        "the participant socket {link} leaked into the worker's shell or its child: {listing}"
    );
}

#[test]
fn uuids_are_normalised() {
    assert_eq!(
        replace_uuids("task 0f8fad5b-d9cb-469f-a165-70867728950e done ✓"),
        "task <ID> done ✓"
    );
    assert_eq!(replace_uuids("not-a-uuid"), "not-a-uuid");
}

// ---------------------------------------------------------------------------
// Hop latency: the baseline ADR-0020's latency criterion compares against
// ---------------------------------------------------------------------------

/// What the leader logs as it calls `invoke_agent`.
const CALL_EVENT: &str = "swarm kit: invoke_agent call";
/// What the broker logs as it hands the child its `task` frame
/// (`LocalRunner::run_task`, right after `submit_task`).
const TASK_FRAME_EVENT: &str = "Delegated a task to a local worker";

/// Records when each [`CALL_EVENT`] and [`TASK_FRAME_EVENT`] was logged.
#[derive(Clone, Default)]
struct HopClock(std::sync::Arc<parking_lot::Mutex<Vec<(std::time::Instant, bool)>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for HopClock {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let at = std::time::Instant::now();
        let mut message = Message(String::new());
        event.record(&mut message);
        if message.0 == CALL_EVENT || message.0 == TASK_FRAME_EVENT {
            self.0.lock().push((at, message.0 == CALL_EVENT));
        }
    }
}

/// Delegate `n` one-reply tasks in turn and return each hop: from the
/// leader's `invoke_agent` call to the broker handing the child its `task`
/// frame, both read off tracing timestamps. On `main` before BI-3 a hop
/// spawns a worker process and waits for it to register; BI-8 measures the
/// new path with this same function.
///
/// Runs on the calling thread's tracing dispatcher, so call it from a
/// current-thread runtime: the broker's tasks then log on this thread too.
pub(crate) async fn measure_hop_latency(n: usize) -> Vec<Duration> {
    use tracing_subscriber::layer::SubscriberExt;

    let clock = HopClock::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(clock.clone()));

    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(WORKER_MODEL, (0..n).map(|_| Reply::text("done"))),
        Script::new(),
    )
    .await;
    for _ in 0..n {
        tracing::info!("{CALL_EVENT}");
        let run = kit.run_leader("hop").await;
        assert!(run.output.is_ok(), "{:?}", run.output);
    }

    let events = clock.0.lock().clone();
    let hops: Vec<Duration> = events
        .chunks(2)
        .map(|pair| match pair {
            [(called, true), (framed, false)] => framed.duration_since(*called),
            other => panic!("expected a call then a task frame, got {other:?}"),
        })
        .collect();
    assert_eq!(hops.len(), n, "one task frame per call");
    hops
}

/// The pre-BI-3 baseline in `docs/research/fabric-hop-latency-baseline.md`.
/// Ignored: 200 real worker processes take a minute or two. Run with
/// `cargo test -p chatty-tui fabric_hop_latency_baseline -- --ignored --nocapture`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "a measurement, not a check: 200 real worker processes"]
async fn fabric_hop_latency_baseline() {
    let mut hops = measure_hop_latency(200).await;
    hops.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let at = |q: f64| ms(hops[((hops.len() as f64 * q).ceil() as usize).saturating_sub(1)]);
    let mean = hops.iter().map(|d| ms(*d)).sum::<f64>() / hops.len() as f64;
    println!(
        "hops={} p50={:.1}ms p95={:.1}ms mean={:.1}ms min={:.1}ms max={:.1}ms",
        hops.len(),
        at(0.50),
        at(0.95),
        mean,
        ms(hops[0]),
        ms(hops[hops.len() - 1])
    );
}

// ---------------------------------------------------------------------------
// Calls over the worker's connection (BI-4, AGE-636; invariants 4 and 11)
// ---------------------------------------------------------------------------

const MIDDLE: &str = "kit-middle";
const MIDDLE_MODEL: &str = "kit/middle";
const GRANDCHILD: &str = "kit-grandchild";
const GRANDCHILD_MODEL: &str = "kit/grandchild";

/// Leader → middle worker → grandchild, on separate endpoints (BI-6 owns
/// one shared budget-1 endpoint). The middle worker is a sub-leader that
/// reads the directory, then delegates over its connection; `grandchild`
/// is its model's replies.
async fn nested_kit(grandchild: Vec<Reply>) -> SwarmKit {
    SwarmKit::start(
        vec![
            AgentDef::new(MIDDLE, MIDDLE_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(GRANDCHILD, GRANDCHILD_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            MIDDLE_MODEL,
            [
                Reply::tool_call("list_agents", serde_json::json!({})),
                Reply::tool_call(
                    "invoke_agent",
                    serde_json::json!({ "agent": GRANDCHILD, "prompt": "read the readme" }),
                ),
                Reply::text("The grandchild read it."),
            ],
        ),
        Script::new().route(GRANDCHILD_MODEL, grandchild),
    )
    .await
}

/// The grandchild's replies in the nested tests that complete.
fn reading_grandchild() -> Vec<Reply> {
    vec![
        Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" })),
        Reply::text("It says Chatty."),
    ]
}

/// The progress lines, in order: what the leader's transcript shows.
fn progress_lines(run: &LeaderRun) -> Vec<String> {
    run.progress
        .iter()
        .filter_map(|p| match p {
            InvokeAgentProgress::Step(line) => Some(line.clone()),
            _ => None,
        })
        .collect()
}

/// BI-4: a worker with no broker of its own delegates over the connection
/// its broker made, and the grandchild's tool events reach the leader,
/// rendered by `progress_text_for_event` at the grandchild and passed up
/// unchanged by the middle worker.
#[tokio::test]
async fn nested_delegation_over_the_connection() {
    use chatty_core::session::SessionEvent;
    use chatty_core::tools::progress_text_for_event;

    let kit = nested_kit(reading_grandchild()).await;
    let run = kit
        .run_leader("ask the grandchild to read the readme")
        .await;

    let out = run
        .output
        .as_ref()
        .expect("the nested delegation succeeded");
    assert!(out.success);
    assert_eq!(out.response, "The grandchild read it.");

    // The grandchild really ran: its read reached its model, and its answer
    // reached the middle worker's.
    let grandchild = kit.ndjson.requests_for(GRANDCHILD_MODEL);
    assert_eq!(grandchild.len(), 2);
    assert!(String::from_utf8_lossy(&grandchild[1].body).contains("# Chatty"));
    let middle = kit.sse.requests_for(MIDDLE_MODEL);
    assert_eq!(middle.len(), 3);
    assert!(
        String::from_utf8_lossy(&middle[1].body).contains(GRANDCHILD),
        "list_agents over the connection listed the grandchild's role"
    );
    assert!(String::from_utf8_lossy(&middle[2].body).contains("It says Chatty."));

    // The grandchild's own lines, as progress_text_for_event renders its
    // events, sit inside the middle worker's invoke_agent at the leader.
    let mut names = HashMap::new();
    let grandchild_lines: Vec<String> = [
        SessionEvent::ToolCallStarted {
            id: "call-1".to_string(),
            name: "read_file".to_string(),
        },
        SessionEvent::ToolCallResult {
            id: "call-1".to_string(),
            result: "# Chatty".to_string(),
        },
    ]
    .iter()
    .filter_map(|event| progress_text_for_event(event, &mut names))
    .collect();
    assert_eq!(grandchild_lines, ["read_file", "\u{2713} read_file"]);
    assert_eq!(
        progress_lines(&run),
        [
            "list_agents",
            "\u{2713} list_agents",
            "invoke_agent",
            "read_file",
            "\u{2713} read_file",
            "\u{2713} invoke_agent",
        ],
        "the leader sees the grandchild's steps inside the middle worker's delegation"
    );

    // One edge-log row per call, named by the connections: the root's call
    // to the middle worker, and the middle worker's to the grandchild.
    let log = std::fs::read_to_string(kit.broker().edge_log_path()).expect("the edge log");
    let rows: Vec<serde_json::Value> = log
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON row"))
        .collect();
    let edges: Vec<(&str, &str, &str, &str)> = rows
        .iter()
        .map(|row| {
            (
                row["kind"].as_str().unwrap(),
                row["from"].as_str().unwrap(),
                row["to"].as_str().unwrap(),
                row["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        edges,
        [
            ("task", "kit-middle-0", "kit-grandchild-0", "completed"),
            ("task", "root", "kit-middle-0", "completed"),
        ]
    );
}

/// A raw loopback HTTP GET against the broker's gateway, for the counter's
/// own sanity check.
async fn http_get(port: u16, path: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("the gateway is listening");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

/// Invariant 4: across the nested delegation — the leader's call, the
/// middle worker's directory read and its call to the grandchild — no
/// request for a role or the directory reaches the gateway's HTTP side.
#[tokio::test]
async fn no_worker_call_uses_loopback() {
    let kit = nested_kit(reading_grandchild()).await;
    let routes = kit.broker().route_counter();

    let run = kit
        .run_leader("ask the grandchild to read the readme")
        .await;
    assert!(run.output.is_ok(), "{:?}", run.output);
    assert_eq!(kit.sse.requests_for(MIDDLE_MODEL).len(), 3);
    assert_eq!(kit.ndjson.requests_for(GRANDCHILD_MODEL).len(), 2);

    assert_eq!(
        routes.role_requests(),
        0,
        "a role was reached over loopback"
    );
    assert_eq!(
        routes.directory_requests(),
        0,
        "the directory was read over loopback"
    );

    // The counter counts: the same role and the directory, over HTTP — both
    // refused now (BI-7), never served.
    let port = kit.broker().port;
    assert!(
        http_get(port, &format!("/a2a/{GRANDCHILD}/.well-known/agent.json"))
            .await
            .contains("403 Forbidden")
    );
    assert!(
        http_get(port, "/.well-known/agent.json")
            .await
            .contains("403 Forbidden")
    );
    assert_eq!(routes.role_requests(), 1);
    assert_eq!(routes.directory_requests(), 1);
}

/// Every process descended from this test process whose `HOME` is `kit`'s:
/// the kit's workers and whatever they started.
fn subtree(kit: &SwarmKit) -> Vec<i32> {
    let home = format!("HOME={}", kit.root().join("home").display());
    let mut parents = HashMap::new();
    for entry in std::fs::read_dir("/proc").expect("/proc is readable") {
        let path = entry.expect("a /proc entry").path();
        let Some(pid) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Some(ppid) = std::fs::read_to_string(path.join("status"))
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|l| l.strip_prefix("PPid:\t").map(str::to_string))
            })
            .and_then(|p| p.trim().parse::<i32>().ok())
        else {
            continue;
        };
        parents.insert(pid, ppid);
    }
    let me = std::process::id() as i32;
    let descends = |mut pid: i32| {
        while let Some(&parent) = parents.get(&pid) {
            if parent == me {
                return true;
            }
            pid = parent;
        }
        false
    };
    parents
        .keys()
        .copied()
        .filter(|&pid| descends(pid))
        .filter(|&pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|var| var == home.as_bytes()))
        })
        .collect()
}

/// Whether `pid` still exists — a zombie included, so a process that died
/// but was never reaped still counts.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Invariant 11: cancelling the leader's task reaps every process in its
/// subtree — the middle worker and the grandchild it delegated to over its
/// connection — within 5 s.
#[tokio::test]
async fn cancel_reaps_the_subtree() {
    // The grandchild waits on its model for far longer than the test runs.
    let kit = nested_kit(vec![Reply::Delay(120_000), Reply::text("too late")]).await;
    let tool = kit.leader_tool();
    let leader = tokio::spawn(async move {
        tool.call(
            &mut ToolContext::new(),
            InvokeAgentArgs {
                agent: MIDDLE.to_string(),
                prompt: "ask the grandchild".to_string(),
                include_trace: false,
            },
        )
        .await
    });

    // Both workers up, the grandchild mid-request.
    let deadline = std::time::Instant::now() + DEADLINE;
    let pids = loop {
        let pids = subtree(&kit);
        if pids.len() >= 2 && !kit.ndjson.requests_for(GRANDCHILD_MODEL).is_empty() {
            break pids;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the subtree never came up: {pids:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };

    leader.abort();
    let cancelled = std::time::Instant::now();
    while pids.iter().any(|&pid| alive(pid)) {
        assert!(
            cancelled.elapsed() < Duration::from_secs(5),
            "still alive 5 s after the cancel: {:?}",
            pids.iter().filter(|&&pid| alive(pid)).collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(subtree(&kit).is_empty(), "nothing new was started either");
}

// ---------------------------------------------------------------------------
// Tree messages (TM-1, AGE-654; fabric spec 5 invariants 3 and 7)
// ---------------------------------------------------------------------------

const SENDER: &str = "kit-sender";
const SENDER_MODEL: &str = "kit/sender";
const SIBLING: &str = "kit-sibling";
const SIBLING_MODEL: &str = "kit/sibling";

/// The tool results a request carries, in order: what the worker's tools
/// handed its model.
fn tool_results(request: &RecordedRequest) -> Vec<serde_json::Value> {
    request.json()["messages"]
        .as_array()
        .expect("a chat request has messages")
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| {
            let content = message["content"].as_str().expect("a tool result is text");
            serde_json::from_str(content).unwrap_or_else(|_| serde_json::json!(content))
        })
        .collect()
}

/// The names of the tools a request offered the model.
fn offered_tools(request: &RecordedRequest) -> Vec<String> {
    request.json()["tools"]
        .as_array()
        .expect("the request offers tools")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

/// The edge log's message rows, as `(from, to, bytes, outcome)`.
fn message_rows(kit: &SwarmKit) -> Vec<(String, String, u64, String)> {
    std::fs::read_to_string(kit.broker().edge_log_path())
        .expect("the edge log")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("a JSON row"))
        .filter(|row| row["kind"] == "message")
        .map(|row| {
            (
                row["from"].as_str().unwrap().to_string(),
                row["to"].as_str().unwrap().to_string(),
                row["bytes"].as_u64().unwrap(),
                row["outcome"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Invariant 3: a worker's message to a sibling — another worker of the
/// same owner, live at the time — is refused as `not_on_tree`, and the
/// sibling's model never sees it. The same worker's message to its owner
/// is accepted, so the refusal is about the recipient, not the tool.
#[tokio::test]
async fn sibling_refused() {
    const SECRET: &str = "the sibling must never read this";
    let sibling = format!("{SIBLING}-0");
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(SENDER, SENDER_MODEL, Endpoint::Sse),
            AgentDef::new(SIBLING, SIBLING_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            SENDER_MODEL,
            [
                Reply::tool_call(
                    "send_message",
                    serde_json::json!({ "to": sibling, "text": SECRET }),
                ),
                Reply::tool_call(
                    "send_message",
                    serde_json::json!({ "to": "root", "text": "halfway there" }),
                ),
                Reply::text("Sent."),
            ],
        ),
        // The sibling sits on its first model call long enough for the
        // sender to run in full, then reads a file and answers: two
        // requests after the message was sent.
        Script::new().route(
            SIBLING_MODEL,
            [
                Reply::Delay(8_000),
                Reply::tool_call("read_file", serde_json::json!({ "path": "README.md" })),
                Reply::text("The sibling finished."),
            ],
        ),
    )
    .await;

    let sibling_run = kit.run_leader_to(SIBLING, "read the readme");
    let sender_run = async {
        // The sibling is up and mid-call before the sender starts.
        let deadline = std::time::Instant::now() + DEADLINE;
        while kit.ndjson.requests_for(SIBLING_MODEL).is_empty() {
            assert!(
                std::time::Instant::now() < deadline,
                "the sibling never ran"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let run = kit.run_leader_to(SENDER, "tell someone").await;
        assert!(
            kit.participants().is_registered(&sibling),
            "the sibling was still live when the message was sent"
        );
        run
    };
    let (sibling_run, sender_run) = tokio::join!(sibling_run, sender_run);

    let out = sender_run.output.as_ref().expect("the sender's delegation");
    assert_eq!(out.response, "Sent.");
    let sender = kit.sse.requests_for(SENDER_MODEL);
    assert_eq!(sender.len(), 3);
    assert_eq!(
        tool_results(&sender[2]),
        [
            serde_json::json!({ "status": "refused", "reason": "not_on_tree" }),
            serde_json::json!({ "status": "pending", "id": "msg-1" }),
        ]
    );

    let out = sibling_run
        .output
        .as_ref()
        .expect("the sibling's delegation");
    assert_eq!(out.response, "The sibling finished.");
    let requests = kit.ndjson.requests_for(SIBLING_MODEL);
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(
            !String::from_utf8_lossy(&request.body).contains(SECRET),
            "the sibling's model saw the refused message"
        );
    }

    assert_eq!(
        message_rows(&kit),
        [
            (
                format!("{SENDER}-0"),
                sibling,
                SECRET.len() as u64,
                "refused: not_on_tree".to_string()
            ),
            (
                format!("{SENDER}-0"),
                "root".to_string(),
                "halfway there".len() as u64,
                "pending".to_string()
            ),
        ]
    );
}

/// Anything but the owner is `not_on_tree`: a name nobody has, the sender's
/// own name, and a name that looks like a leader's.
#[tokio::test]
async fn unknown_recipient_refused() {
    let me = format!("{WORKER}-0");
    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse)],
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::tool_call(
                    "send_message",
                    serde_json::json!({ "to": "nobody-7", "text": "hello?" }),
                ),
                Reply::tool_call(
                    "send_message",
                    serde_json::json!({ "to": me, "text": "note to self" }),
                ),
                Reply::tool_call(
                    "send_message",
                    serde_json::json!({ "to": "leader-0", "text": "hello?" }),
                ),
                Reply::text("Nobody answered."),
            ],
        ),
        Script::new(),
    )
    .await;

    let run = kit.run_leader("message someone").await;
    assert_eq!(
        run.output.as_ref().expect("the delegation").response,
        "Nobody answered."
    );
    let requests = kit.sse.requests_for(WORKER_MODEL);
    let refused = serde_json::json!({ "status": "refused", "reason": "not_on_tree" });
    assert_eq!(
        tool_results(requests.last().unwrap()),
        [refused.clone(), refused.clone(), refused]
    );
    let outcomes: Vec<String> = message_rows(&kit).into_iter().map(|row| row.3).collect();
    assert_eq!(outcomes, ["refused: not_on_tree"; 3]);
}

/// A `coder` with an empty `delegates_to` is offered `send_message` — it
/// needs a connection, not delegation rights — and no `invoke_agent`; and
/// the message reaches its owner's pending list.
#[tokio::test]
async fn leaf_worker_has_send_message() {
    let kit = SwarmKit::start(
        vec![AgentDef::new(WORKER, WORKER_MODEL, Endpoint::Sse).profile("coder")],
        Script::new().route(
            WORKER_MODEL,
            [
                Reply::tool_call(
                    "send_message",
                    serde_json::json!({ "to": "root", "text": "tests pass" }),
                ),
                Reply::text("Done."),
            ],
        ),
        Script::new(),
    )
    .await;

    let run = kit.run_leader("fix it").await;
    assert_eq!(
        run.output.as_ref().expect("the delegation").response,
        "Done."
    );
    let requests = kit.sse.requests_for(WORKER_MODEL);
    let tools = offered_tools(&requests[0]);
    assert!(tools.iter().any(|t| t == "send_message"), "{tools:?}");
    assert!(!tools.iter().any(|t| t == "invoke_agent"), "{tools:?}");
    assert!(!tools.iter().any(|t| t == "list_agents"), "{tools:?}");
    assert_eq!(
        tool_results(&requests[1]),
        [serde_json::json!({ "status": "pending", "id": "msg-1" })]
    );
}

// ---------------------------------------------------------------------------
// Spawn context: sub-leaders use the root broker (BI-5, AGE-637; ADR-0020
// invariants 5 and 6)
// ---------------------------------------------------------------------------

const LEAD: &str = "kit-lead";
const LEAD_MODEL: &str = "kit/lead";

/// Invariant 5: a sub-leader's child branches from the sub-leader's branch,
/// and its evidence diffs against that branch. The sub-leader commits
/// `lead.txt` on its own branch (with the git tools), then delegates; the grandchild writes
/// `grandchild.txt`. The grandchild's branch forks at the sub-leader's tip,
/// its tree lies inside the sub-leader's, and the evidence the sub-leader
/// reads names the sub-leader's branch as its base and shows only the
/// grandchild's own commit.
#[tokio::test]
async fn grandchild_branches_from_its_subleader() {
    let kit = SwarmKit::start_in_repo(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(GRANDCHILD, GRANDCHILD_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            LEAD_MODEL,
            [
                Reply::tool_call(
                    "write_file",
                    serde_json::json!({ "path": "lead.txt", "content": "lead\n" }),
                ),
                Reply::tool_call("git_add", serde_json::json!({ "paths": ["lead.txt"] })),
                Reply::tool_call("git_commit", serde_json::json!({ "message": "lead" })),
                Reply::tool_call(
                    "invoke_agent",
                    serde_json::json!({ "agent": GRANDCHILD, "prompt": "write grandchild.txt" }),
                ),
                Reply::text("The grandchild wrote it."),
            ],
        ),
        Script::new().route(
            GRANDCHILD_MODEL,
            [
                Reply::tool_call(
                    "write_file",
                    serde_json::json!({ "path": "grandchild.txt", "content": "grandchild\n" }),
                ),
                Reply::text("Wrote grandchild.txt."),
            ],
        ),
    )
    .await;

    let run = kit.run_leader("have the grandchild write a file").await;
    let out = run
        .output
        .as_ref()
        .expect("the nested delegation succeeded");
    assert_eq!(
        out.response.lines().next(),
        Some("The grandchild wrote it.")
    );

    let workspace = kit.workspace();
    let lead_branch = format!("sub-agent/{LEAD}-0");
    let grandchild_branch = format!("sub-agent/{GRANDCHILD}-0");
    let lead_tip = git(&workspace, &["rev-parse", &lead_branch]);
    assert_ne!(
        lead_tip,
        git(&workspace, &["rev-parse", "main"]),
        "the sub-leader committed on its own branch before it delegated"
    );
    assert_eq!(
        git(
            &workspace,
            &["merge-base", &grandchild_branch, &lead_branch]
        ),
        lead_tip,
        "the grandchild's branch forks at the sub-leader's tip, not at the root's HEAD"
    );
    assert_eq!(
        git(
            &workspace,
            &[
                "log",
                "--format=%s",
                &format!("{lead_branch}..{grandchild_branch}")
            ]
        )
        .lines()
        .count(),
        1,
        "the grandchild's branch carries its own commit and nothing else"
    );
    let lead_tree = workspace
        .join(".chatty/worktrees")
        .join(format!("{LEAD}-0"));
    assert!(
        lead_tree
            .join(".chatty/worktrees")
            .join(format!("{GRANDCHILD}-0"))
            .join("grandchild.txt")
            .is_file(),
        "the grandchild's tree lies inside the sub-leader's"
    );

    // The evidence the sub-leader's model read: measured against its own
    // branch, so it shows the grandchild's file and not the sub-leader's.
    let lead_requests = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead_requests.len(), 5);
    let body = String::from_utf8_lossy(&lead_requests[4].body).replace("\\n", "\n");
    let at = body
        .find("```evidence")
        .unwrap_or_else(|| panic!("the grandchild's evidence reached the sub-leader: {body}"));
    let evidence = &body[at..at + body[at + 3..].find("```").expect("a closed block") + 6];
    assert!(
        evidence.contains(&format!("branch: {grandchild_branch}"))
            && evidence.contains(&format!("base: {lead_branch}"))
            && evidence.contains("commits: 1"),
        "{evidence}"
    );
    assert!(
        evidence.contains("grandchild.txt") && !evidence.contains("lead.txt"),
        "the diff holds only the grandchild's own commit: {evidence}"
    );
}

/// Invariant 6: a spawn context that reaches outside the calling node's own
/// is refused, naming the field, and nothing is spawned. The caller is a
/// node on a connection the root broker made, whose own context is the
/// kit's workspace and a roster of one; it calls over the wire with a
/// context of its own making.
#[tokio::test]
async fn spawn_context_is_clamped() {
    use chatty_fabric::{CallError, CallEvent, CallRequest, InvokeAgentParams, SpawnContext};
    use chatty_protocol_gateway::participant::{DelegatedTask, open_connection};
    use chatty_protocol_gateway::worker::{WorkerConnection, worker_card};
    use futures::StreamExt;

    const HELPER: &str = "kit-helper";
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse),
            AgentDef::new(HELPER, "kit/helper", Endpoint::Ndjson),
        ],
        Script::new(),
        Script::new(),
    )
    .await;
    let registry = kit.participants();
    let connection = open_connection(&registry, "rogue").expect("a connection");
    let name = connection.name.clone();
    connection.worker_end.set_nonblocking(true).unwrap();
    let worker = WorkerConnection::connect(
        tokio::net::UnixStream::from_std(connection.worker_end).unwrap(),
        worker_card("test"),
    )
    .await
    .expect("welcomed");
    let transport = worker.transport();
    while !registry.is_registered(&name) {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let tree = kit.workspace().to_string_lossy().into_owned();
    registry.set_node_context(
        &name,
        SpawnContext {
            workspace_root: Some(tree.clone()),
            base_branch: None,
            roster: vec![HELPER.to_string()],
            verification: None,
            endpoint: None,
        },
    );
    // The context the broker would derive for the helper: the caller's own,
    // with the root's endpoint for it. Each case widens one field.
    let own = SpawnContext {
        workspace_root: Some(tree),
        base_branch: None,
        roster: vec![HELPER.to_string()],
        verification: None,
        endpoint: Some(kit.ndjson.base_url()),
    };
    let outside = SpawnContext {
        workspace_root: Some(kit.root().to_string_lossy().into_owned()),
        ..own.clone()
    };
    let widened = SpawnContext {
        roster: vec![HELPER.to_string(), LEAD.to_string()],
        ..own.clone()
    };
    let overridden = SpawnContext {
        endpoint: Some("http://127.0.0.1:9/v1".to_string()),
        ..own.clone()
    };

    let (_task, _updates) = registry
        .submit_task(&name, DelegatedTask::new("widen your context"))
        .expect("the rogue node is connected");
    let refusals = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let seen = refusals.clone();
    tokio::time::timeout(
        DEADLINE,
        worker.serve_one_task(move |_task, _sink, _inputs| async move {
            for context in [outside, widened, overridden] {
                let mut call = transport
                    .call(CallRequest::InvokeAgent(InvokeAgentParams {
                        agent: HELPER.to_string(),
                        prompt: "hi".to_string(),
                        handle: None,
                        include_trace: false,
                        spawn_context: Some(context),
                    }))
                    .await
                    .expect("the call goes out");
                match call.next().await {
                    Some(Err(CallError::SpawnContextRefused { field, .. })) => {
                        seen.lock().push(field)
                    }
                    Some(Ok(CallEvent::Result(result))) => panic!("accepted: {result}"),
                    other => panic!("expected a refusal naming the field, got {other:?}"),
                }
            }
            Ok(())
        }),
    )
    .await
    .expect("the calls finish")
    .expect("the task ran");

    assert_eq!(
        *refusals.lock(),
        ["workspace_root", "roster", "endpoint"],
        "each widened field is refused by name"
    );
    assert!(
        kit.ndjson.requests().is_empty() && registry.names() == [name],
        "nothing was spawned for a refused context"
    );
}

/// Usage folds across two hops through the root (AGE-415): the leader's
/// total is the sum of what the fake servers reported for the sub-leader's
/// and its child's model calls, and it is the total the pre-change golden
/// recorded for the same scenario (BI-0).
#[tokio::test]
async fn usage_folds_across_two_hops() {
    use chatty_core::testing::fake_model::DEFAULT_USAGE;

    let scenario = kit_scenarios()
        .into_iter()
        .find(|s| s.name == "nested_delegation_separate_endpoints")
        .expect("the scenario exists");
    let (kit, run) = run_scenario(scenario).await;
    assert!(run.output.is_ok(), "{:?}", run.output);

    let usage = run
        .progress
        .iter()
        .find_map(|p| match p {
            InvokeAgentProgress::Finished { usage, .. } => Some(usage.clone()),
            _ => None,
        })
        .expect("the delegation finished");
    let folded = (
        usage.iter().map(|u| u.input_tokens as u64).sum::<u64>(),
        usage.iter().map(|u| u.output_tokens as u64).sum::<u64>(),
    );

    // Every request the fake servers answered reported the default usage:
    // the sub-leader's two and the helper's one.
    let requests = kit.sse.requests().len() + kit.ndjson.requests().len();
    assert_eq!(kit.sse.requests_for("kit/lead").len(), 2);
    assert_eq!(kit.ndjson.requests_for("kit/helper").len(), 1);
    let reported = (
        DEFAULT_USAGE.0 * requests as u64,
        DEFAULT_USAGE.1 * requests as u64,
    );
    assert_eq!(folded, reported, "the root's total is every hop's usage");

    let golden =
        std::fs::read_to_string(pre_fabric_dir().join("nested_delegation_separate_endpoints.txt"))
            .expect("the pre-change golden");
    assert!(
        golden.contains(&format!("usage=[input={} output={} ", folded.0, folded.1)),
        "the pre-change golden recorded the same total: {golden}"
    );
}

/// Whether `pid` holds a listening socket — a TCP port or a Unix socket
/// accepting connections — among its open descriptors.
fn listens(pid: i32) -> bool {
    let inodes: std::collections::HashSet<String> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|dir| {
            dir.filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
                .filter_map(|link| {
                    let link = link.to_string_lossy().into_owned();
                    link.strip_prefix("socket:[")?
                        .strip_suffix(']')
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    let net = |table: &str| std::fs::read_to_string(format!("/proc/{pid}/net/{table}"));
    // `st` 0A is LISTEN; the inode is the tenth column.
    let tcp_listening = ["tcp", "tcp6"].into_iter().any(|table| {
        net(table).is_ok_and(|text| {
            text.lines().skip(1).any(|line| {
                let cols: Vec<&str> = line.split_whitespace().collect();
                cols.get(3) == Some(&"0A") && cols.get(9).is_some_and(|i| inodes.contains(*i))
            })
        })
    });
    // Flags 00010000 is __SO_ACCEPTCON: a listening Unix socket.
    let unix_listening = net("unix").is_ok_and(|text| {
        text.lines().skip(1).any(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            cols.get(3) == Some(&"00010000") && cols.get(6).is_some_and(|i| inodes.contains(*i))
        })
    });
    tcp_listening || unix_listening
}

/// ADR-0020: one broker per root. While a sub-leader's child is mid-turn,
/// the root — this test process — is the only process of the swarm that
/// listens on anything: neither the sub-leader nor its child binds a
/// gateway port or a participant socket, and the only socket file under the
/// kit's runtime directory is the root's.
#[tokio::test]
async fn one_broker_per_root() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(GRANDCHILD, GRANDCHILD_MODEL, Endpoint::Ndjson),
        ],
        Script::new().route(
            LEAD_MODEL,
            [
                Reply::tool_call("list_agents", serde_json::json!({})),
                Reply::tool_call(
                    "invoke_agent",
                    serde_json::json!({ "agent": GRANDCHILD, "prompt": "take your time" }),
                ),
                Reply::text("Done."),
            ],
        ),
        // Mid-turn long enough to look at the process tree.
        Script::new().route(GRANDCHILD_MODEL, [Reply::Delay(1500), Reply::text("done")]),
    )
    .await;
    assert!(
        listens(std::process::id() as i32),
        "the root holds the one broker"
    );

    let (seen, run) = tokio::join!(
        async {
            let deadline = std::time::Instant::now() + DEADLINE;
            loop {
                let pids = subtree(&kit);
                if pids.len() >= 2 && !kit.ndjson.requests_for(GRANDCHILD_MODEL).is_empty() {
                    let listening: Vec<i32> =
                        pids.iter().copied().filter(|&pid| listens(pid)).collect();
                    break (pids, listening);
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the nested run never came up: {pids:?}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        },
        kit.run_leader("ask the grandchild")
    );
    let (pids, listening) = seen;
    assert!(run.output.is_ok(), "{:?}", run.output);
    assert!(pids.len() >= 2, "the sub-leader and its child were both up");
    assert!(
        listening.is_empty(),
        "a worker of the swarm runs a broker of its own: {listening:?} of {pids:?}"
    );

    let mut sockets = Vec::new();
    let mut dirs = vec![kit.root().join("run")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "sock") {
                sockets.push(path);
            }
        }
    }
    assert_eq!(
        sockets,
        [kit.socket()],
        "only the root binds a participant socket"
    );
}

// ---------------------------------------------------------------------------
// Endpoint permits per run (BI-6, AGE-638; ADR-0020 invariants 7–9)
// ---------------------------------------------------------------------------

/// Invariant 8: at budget 1, a sub-leader and its child on the same
/// endpoint both complete. The sub-leader's run holds the endpoint's one
/// permit while it talks to its model, lets go of it while its call to the
/// child is outstanding, and gets it back before the child's answer reaches
/// it.
#[tokio::test]
async fn nested_delegation_at_budget_one_completes() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new(LEAD, LEAD_MODEL, Endpoint::Sse).sub_leader(),
            AgentDef::new(GRANDCHILD, GRANDCHILD_MODEL, Endpoint::Sse),
        ],
        Script::new()
            .route(
                LEAD_MODEL,
                [
                    Reply::tool_call(
                        "invoke_agent",
                        serde_json::json!({ "agent": GRANDCHILD, "prompt": "read the readme" }),
                    ),
                    Reply::text("The grandchild read it."),
                ],
            )
            .route(GRANDCHILD_MODEL, reading_grandchild()),
        Script::new(),
    )
    .await;
    assert_eq!(
        ModuleSettingsModel::default().default_endpoint_budget,
        1,
        "the case under test is the default budget of one"
    );

    let run = kit
        .run_leader("ask the grandchild to read the readme")
        .await;

    let out = run.output.as_ref().expect("the leader's call succeeded");
    assert!(out.success, "{out:?}");
    assert_eq!(out.response, "The grandchild read it.");
    // What the sub-leader's model was told the child said: the child's
    // answer, not a refusal to start it.
    let lead = kit.sse.requests_for(LEAD_MODEL);
    assert_eq!(lead.len(), 2);
    let told = String::from_utf8_lossy(&lead[1].body);
    assert!(
        told.contains("It says Chatty."),
        "the sub-leader's call to its child on its own endpoint did not complete; \
         its model was told: {told}"
    );
    // Both ran, and never at once.
    assert_eq!(kit.sse.requests_for(GRANDCHILD_MODEL).len(), 2);
    assert_eq!(kit.sse.max_concurrency(), 1);
}

/// A leaf: waits `ms` on its model, then answers `text`.
fn slow_leaf(ms: u64, text: &str) -> Vec<Reply> {
    vec![Reply::Delay(ms), Reply::text(text)]
}

/// One `invoke_agent` call to `agent`, then `answer`.
fn delegate(agent: &str, answer: &str) -> Vec<Reply> {
    vec![
        Reply::tool_call(
            "invoke_agent",
            serde_json::json!({ "agent": agent, "prompt": "do your part" }),
        ),
        Reply::text(answer),
    ]
}

/// Invariant 7: across a 3-level, 6-worker run on two budget-1 endpoints,
/// no endpoint ever serves more model requests at once than its limit.
///
/// The root starts two chains at once, each crossing both endpoints:
/// `kit-p` (SSE) → `kit-q` (NDJSON) → `kit-s` (SSE), and `kit-r` (NDJSON) →
/// `kit-t` (SSE) → `kit-u` (NDJSON). Every sub-leader releases its slot while
/// its child works, so the four slots change hands down both chains. `kit-s`
/// answers well before `kit-u`, whose slow request holds the NDJSON slot:
/// `kit-q` must wait for it before its next model call rather than talk to
/// its model beside `kit-u`.
#[tokio::test]
async fn permits_never_oversubscribe() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new("kit-p", "kit/p", Endpoint::Sse).sub_leader(),
            AgentDef::new("kit-q", "kit/q", Endpoint::Ndjson).sub_leader(),
            AgentDef::new("kit-r", "kit/r", Endpoint::Ndjson).sub_leader(),
            AgentDef::new("kit-s", "kit/s", Endpoint::Sse),
            AgentDef::new("kit-t", "kit/t", Endpoint::Sse).sub_leader(),
            AgentDef::new("kit-u", "kit/u", Endpoint::Ndjson),
        ],
        Script::new()
            .route("kit/p", delegate("kit-q", "P done."))
            .route("kit/t", delegate("kit-u", "T done."))
            .route("kit/s", slow_leaf(300, "S done.")),
        Script::new()
            .route("kit/q", delegate("kit-s", "Q done."))
            .route("kit/r", delegate("kit-t", "R done."))
            .route("kit/u", slow_leaf(2500, "U done.")),
    )
    .await;

    let (left, right) = tokio::join!(
        kit.run_leader_to("kit-p", "run your chain"),
        kit.run_leader_to("kit-r", "run your chain"),
    );

    for (run, answer) in [(&left, "P done."), (&right, "R done.")] {
        let out = run.output.as_ref().expect("the leader's call succeeded");
        assert!(out.success, "{out:?}");
        assert_eq!(out.response, answer);
    }
    // Every worker ran to its answer: two requests for each sub-leader, one
    // for each leaf.
    for (daemon, model, requests) in [
        (&kit.sse, "kit/p", 2),
        (&kit.ndjson, "kit/q", 2),
        (&kit.ndjson, "kit/r", 2),
        (&kit.sse, "kit/s", 1),
        (&kit.sse, "kit/t", 2),
        (&kit.ndjson, "kit/u", 1),
    ] {
        assert_eq!(daemon.requests_for(model).len(), requests, "{model}");
    }
    assert_eq!(
        kit.sse.max_concurrency(),
        1,
        "the SSE endpoint's limit is 1"
    );
    assert_eq!(
        kit.ndjson.max_concurrency(),
        1,
        "the NDJSON endpoint's limit is 1"
    );
}

/// Three runs waiting on one busy endpoint are served in the order they
/// arrived: the endpoint's queue is FIFO.
#[tokio::test]
async fn permit_queue_is_fifo() {
    let kit = SwarmKit::start(
        vec![
            AgentDef::new("kit-w", "kit/w", Endpoint::Sse),
            AgentDef::new("kit-x", "kit/x", Endpoint::Sse),
            AgentDef::new("kit-y", "kit/y", Endpoint::Sse),
            AgentDef::new("kit-z", "kit/z", Endpoint::Sse),
        ],
        Script::new()
            // Holds the one slot long enough for the other three to queue.
            .route("kit/w", slow_leaf(1500, "W done."))
            .route("kit/x", [Reply::text("X done.")])
            .route("kit/y", [Reply::text("Y done.")])
            .route("kit/z", [Reply::text("Z done.")]),
        Script::new(),
    )
    .await;

    let after = |ms: u64, agent: &'static str| {
        let kit = &kit;
        async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            kit.run_leader_to(agent, "go").await
        }
    };
    // W takes the slot at once; X, Y and Z queue behind it, in that order
    // (an in-process call reaches the queue within the stagger).
    let (w, x, y, z) = tokio::join!(
        after(0, "kit-w"),
        after(300, "kit-x"),
        after(600, "kit-y"),
        after(900, "kit-z"),
    );
    for run in [&w, &x, &y, &z] {
        assert!(
            run.output.as_ref().is_ok_and(|o| o.success),
            "{:?}",
            run.output
        );
    }

    let first = |model: &str| {
        let requests = kit.sse.requests_for(model);
        assert_eq!(requests.len(), 1, "{model}");
        requests[0].arrived
    };
    let (w, x, y, z) = (
        first("kit/w"),
        first("kit/x"),
        first("kit/y"),
        first("kit/z"),
    );
    assert!(
        w < x && x < y && y < z,
        "served in arrival order: w {w:?}, x {x:?}, y {y:?}, z {z:?}"
    );
    assert_eq!(kit.sse.max_concurrency(), 1);
}

/// Poll `done` until it holds; fail naming `what` at the deadline.
async fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + DEADLINE;
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} never happened"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Invariant 9: a run whose caller gives up while the run waits for its
/// permit makes no model request.
///
/// `kit-m` (SSE) delegates to `kit-g` (NDJSON), which releases `kit-m`'s
/// slot. `kit-b` takes it and holds it on a slow model request. `kit-g`
/// answers, so `kit-m` must re-acquire before its result is delivered, and
/// waits behind `kit-b`. The leader gives up on `kit-m` then. Once `kit-b`
/// is done the slot is free, and `kit-m` still never asks its model again.
#[tokio::test]
async fn cancelled_permit_wait_makes_no_model_call() {
    use chatty_fabric::RunPermitState;

    let kit = SwarmKit::start(
        vec![
            AgentDef::new("kit-m", "kit/m", Endpoint::Sse).sub_leader(),
            AgentDef::new("kit-g", "kit/g", Endpoint::Ndjson),
            AgentDef::new("kit-b", "kit/b", Endpoint::Sse),
        ],
        Script::new()
            .route(
                "kit/m",
                [
                    Reply::tool_call(
                        "invoke_agent",
                        serde_json::json!({ "agent": "kit-g", "prompt": "take your time" }),
                    ),
                    Reply::text("M done."),
                ],
            )
            .route("kit/b", slow_leaf(3000, "B done.")),
        Script::new().route("kit/g", slow_leaf(1000, "G done.")),
    )
    .await;

    let call = |agent: &'static str| {
        let tool = kit.leader_tool();
        tokio::spawn(async move {
            tool.call(
                &mut ToolContext::new(),
                InvokeAgentArgs {
                    agent: agent.to_string(),
                    prompt: "go".to_string(),
                    include_trace: false,
                },
            )
            .await
        })
    };
    let registry = kit.participants();
    let m_state = || registry.node_permit("kit-m-0").map(|p| p.state());

    // kit-m asks for kit-g: its slot is released while kit-g works.
    let m = call("kit-m");
    wait_until("kit-g's model request", || {
        !kit.ndjson.requests_for("kit/g").is_empty()
    })
    .await;
    assert_eq!(m_state(), Some(RunPermitState::Released { outstanding: 1 }));

    // kit-b takes the free slot and holds it.
    let b = call("kit-b");
    wait_until("kit-b's model request", || {
        !kit.sse.requests_for("kit/b").is_empty()
    })
    .await;

    // kit-g answers; kit-m's result waits for the slot kit-b holds.
    wait_until("kit-m waiting for its slot", || {
        m_state() == Some(RunPermitState::Reacquiring { outstanding: 0 })
    })
    .await;
    assert_eq!(kit.sse.requests_for("kit/m").len(), 1);

    // The leader gives up on kit-m.
    m.abort();
    wait_until("kit-m's node to go", || {
        registry.node_permit("kit-m-0").is_none()
    })
    .await;

    // kit-b finishes and frees the slot; nobody is left to take it for kit-m.
    let b = tokio::time::timeout(DEADLINE, b)
        .await
        .expect("kit-b finishes")
        .expect("kit-b's call ran")
        .expect("kit-b's call succeeded");
    assert!(b.success, "{b:?}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        kit.sse.requests_for("kit/m").len(),
        1,
        "kit-m made a model request after its caller gave up"
    );
    assert!(subtree(&kit).is_empty(), "every worker is gone");
}
