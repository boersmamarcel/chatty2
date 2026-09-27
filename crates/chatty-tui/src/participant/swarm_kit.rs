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
    /// Declare `swarm.delegates_to`, so the worker runs a broker of its own
    /// and can delegate in turn.
    pub sub_leader: bool,
}

impl AgentDef {
    pub fn new(name: &str, model: &str, endpoint: Endpoint) -> Self {
        Self {
            name: name.to_string(),
            model: model.to_string(),
            endpoint,
            sub_leader: false,
        }
    }

    pub fn sub_leader(mut self) -> Self {
        self.sub_leader = true;
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
        let root = tempfile::tempdir().expect("a temp dir for the swarm");
        let base = root.path().canonicalize().expect("the temp dir resolves");
        let sse = FakeDaemon::scripted(sse);
        let ndjson = FakeDaemon::scripted(ndjson);

        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace dir");
        std::fs::write(workspace.join("README.md"), "# Chatty\n").expect("README.md");

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
                let mut spec = AgentSpec::named(&agent.name);
                spec.agent.model = Some(agent.model.clone());
                if agent.sub_leader {
                    spec.swarm.delegates_to = vec!["*".to_string()];
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
        let specs = resolve_virtual_agents(
            &models,
            &providers,
            &module_settings,
            &specs,
            &["--auto-approve".to_string()],
        );
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
    /// `invoke_agent` tool, over the root broker.
    pub async fn run_leader_to(&self, agent: &str, prompt: &str) -> LeaderRun {
        let port = self.broker.as_ref().expect("the broker is running").port;
        let tool =
            InvokeAgentTool::new(vec![], vec![], Some(port)).with_local_agents(self.roster.clone());
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

impl Drop for SwarmKit {
    fn drop(&mut self) {
        if let Some(broker) = self.broker.take() {
            broker.shutdown();
        }
    }
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
            InvokeAgentProgress::Text(text) => format!("progress {text}"),
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
            InvokeAgentProgress::Text(text) => Some(text.as_str()),
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
