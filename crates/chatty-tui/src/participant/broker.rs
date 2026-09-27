//! `--broker`: a headless, pipe or interactive leader runs its own protocol
//! gateway so it can delegate to its virtual agents — `local-agent`, or
//! the agent specs `module_settings.virtual_agents` names (ADR-0011 C10)
//! — the way the desktop's module-settings controller does for the GPUI
//! app (AGE-376).
//!
//! This is the same wiring as chatty-gpui's `broker_runner.rs` — one
//! virtual agent per resolved [`VirtualAgentSpec`] that spawns a child per
//! delegated task on a connection the broker makes for it, and the shared
//! participant socket that refuses every registration (ADR-0020) — minus the
//! WASM module registry the desktop's gateway also serves: `--broker`
//! exists to make the workers reachable, not to load modules, so the
//! registry behind it is empty. Module agents chatty-tui already knows
//! about (`--enable`/manifest discovery) are unaffected; they are a separate
//! path from this gateway.
//!
//! Unlike the desktop, which binds a fixed configured port and one
//! well-known socket path, a headless leader is meant to run many at once
//! (benchmarking a team, CI, a script), so both are ephemeral: the HTTP port
//! is OS-assigned, and the socket path is suffixed with this process's pid.
//!
//! A leader configured by flags (`--ollama`, `--openai-compat-url`,
//! `--api-key`) has no config dir a child could read, so those flags are
//! forwarded to every worker (`common_args`); a settings-configured leader
//! forwards nothing, since the child reads the same files.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chatty_core::agent_spec::AgentSpec;
use chatty_core::services::plugin_llm::PluginLlmProvider;
use chatty_core::services::virtual_agents::{VirtualAgentSpec, resolve_virtual_agents};
use chatty_core::services::worker_tree;
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::ProviderConfig;
use chatty_core::tools::worker_executable;
use chatty_fabric::{EdgeLog, Transport};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
#[cfg(test)]
use chatty_protocol_gateway::RouteCounter;
use chatty_protocol_gateway::participant::{
    EndpointBudget, LocalRunner, ParticipantRegistry, TaskEvidence, WorkerWorkspace,
    WorkspaceFactory,
};
use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// The tests' stand-in for the leader's [`PluginLlmProvider`]: the
/// registries they build load no module that calls `llm::complete()`.
#[cfg(test)]
struct NoopProvider;

#[cfg(test)]
impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<chatty_wasm_runtime::Message>,
        _tools: Option<String>,
    ) -> std::result::Result<chatty_wasm_runtime::CompletionResponse, String> {
        Err("chatty-tui --broker runs no WASM modules".to_string())
    }
}

/// The gateway a `--broker` leader runs: an ephemeral HTTP port, a
/// pid-suffixed participant socket, and the virtual agents behind it.
pub struct Broker {
    /// The ephemeral port `invoke_agent`/`list_agents` reach it on.
    pub port: u16,
    socket: PathBuf,
    // Only read by the test-only `participants()` accessor below; the
    // runners it was built for already hold their own clone.
    #[cfg(test)]
    participants: ParticipantRegistry,
    server: JoinHandle<()>,
    participant_listener: JoinHandle<()>,
    /// This process's own handle into the broker: the leader's calls run
    /// on it directly, with no socket and no HTTP hop (ADR-0020, BI-4).
    transport: Arc<dyn Transport>,
    #[cfg(test)]
    routes: RouteCounter,
}

impl Broker {
    /// `workspace_dir` and `auto_approve` mirror the leader's own execution
    /// settings, exactly as the desktop passes its conversation's workspace
    /// and approval mode to `broker_runner::local_runners`: a worker gets
    /// its own `git worktree` under the same root, and inherits the same
    /// no-human approval policy. `provider_flags` are the leader's own
    /// `--ollama`/`--openai-compat-url`/`--api-key`, forwarded verbatim
    /// (see [`provider_flags`]). `agents` is the roster's specs, already
    /// loaded; empty is the one default worker. `leader_model` is the model
    /// this leader runs: what a module's `llm::complete("")` is served by
    /// (PL-H2, AGE-605).
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        leader_model: &ModelConfig,
        models: &[ModelConfig],
        providers: &[ProviderConfig],
        module_settings: &ModuleSettingsModel,
        agents: &[AgentSpec],
        workspace_dir: Option<String>,
        auto_approve: bool,
        provider_flags: &[String],
    ) -> Result<Self> {
        let mut common_args = Vec::new();
        if auto_approve {
            common_args.push("--auto-approve".to_string());
        }
        common_args.extend(provider_flags.iter().cloned());
        let specs =
            resolve_virtual_agents(models, providers, module_settings, agents, &common_args);
        let llm: Arc<dyn LlmProvider> = Arc::new(PluginLlmProvider::new(
            leader_model.clone(),
            models.to_vec(),
            providers.to_vec(),
            tokio::runtime::Handle::current(),
        ));
        Self::serve(
            llm,
            socket_path(),
            worker_executable(),
            module_settings.default_endpoint_budget,
            specs,
            workspace_dir,
            dirs::data_dir(),
        )
        .await
    }

    /// As [`start`](Self::start), but with the participant socket path, the
    /// worker binary and the already-resolved agents given rather than
    /// derived. `start` is the real seam (one process, one pid, one path,
    /// the `chatty-tui` next to this binary); this is the seam tests use so
    /// they neither collide with each other — every test in one binary
    /// shares a pid, so [`socket_path`] alone gives them all the same path
    /// — nor write into the user's real runtime directory, and so they can
    /// spawn a stand-in binary that records its argv. The edge log goes
    /// under the socket's directory ([`Self::edge_log_path`]), which each
    /// test owns.
    #[cfg(test)]
    pub(crate) async fn start_at(
        socket: PathBuf,
        executable: PathBuf,
        default_budget: usize,
        specs: Vec<VirtualAgentSpec>,
        workspace_dir: Option<String>,
    ) -> Result<Self> {
        let data_dir = socket.parent().map(std::path::Path::to_path_buf);
        Self::serve(
            Arc::new(NoopProvider),
            socket,
            executable,
            default_budget,
            specs,
            workspace_dir,
            data_dir,
        )
        .await
    }

    /// Where a [`start_at`](Self::start_at) broker writes its edge log.
    #[cfg(test)]
    pub(crate) fn edge_log_path(&self) -> PathBuf {
        self.socket
            .parent()
            .expect("a test socket has a directory")
            .join("chatty")
            .join("fabric")
            .join(format!("edges-{}.jsonl", std::process::id()))
    }

    /// Bind and serve: the gateway over a module registry whose
    /// `llm::complete()` goes to `provider`. Every call is logged to the
    /// edge log under `data_dir`, when there is one.
    #[allow(clippy::too_many_arguments)]
    async fn serve(
        provider: Arc<dyn LlmProvider>,
        socket: PathBuf,
        executable: PathBuf,
        default_budget: usize,
        specs: Vec<VirtualAgentSpec>,
        workspace_dir: Option<String>,
        data_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let registry = ModuleRegistry::new(provider, ResourceLimits::default())
            .context("failed to build the module registry the broker gateway needs")?;
        let shared = Arc::new(tokio::sync::RwLock::new(registry));
        let mut gateway = ProtocolGateway::new(shared, 0);

        let participants = gateway.participants();
        let listener = chatty_protocol_gateway::participant::bind(&socket)
            .with_context(|| format!("failed to bind participant socket {}", socket.display()))?;
        let participant_listener =
            tokio::spawn(chatty_protocol_gateway::participant::serve(listener));

        for runner in local_runners(
            executable,
            participants.clone(),
            default_budget,
            specs,
            workspace_dir,
        ) {
            gateway = gateway.with_virtual_agent(Arc::new(runner));
        }
        if let Some(dir) = data_dir {
            match EdgeLog::open(&dir) {
                Ok(log) => gateway = gateway.with_edge_log(log),
                Err(error) => tracing::warn!(%error, "The broker's edge log could not be opened"),
            }
        }
        // After every virtual agent: the call path reaches the ones
        // published by now.
        let transport = gateway.transport();
        #[cfg(test)]
        let routes = gateway.route_counter();

        // `gateway.start()` binds its own listener from `self.port`, which
        // leaves no way to learn an OS-assigned port before it is needed
        // below — so bind it here instead, exactly as the broker's own
        // tests (`equivalence.rs`, `input_required_chain.rs`) do.
        let tcp = TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind an ephemeral port for the broker gateway")?;
        let port = tcp
            .local_addr()
            .context("failed to read the broker gateway's bound port")?
            .port();
        let router = gateway.build_router();
        let server = tokio::spawn(async move {
            axum::serve(tcp, router).await.ok();
        });

        Ok(Self {
            port,
            socket,
            #[cfg(test)]
            participants,
            server,
            participant_listener,
            transport,
            #[cfg(test)]
            routes,
        })
    }

    /// This process's direct handle into the broker (ADR-0020, BI-4).
    pub fn transport(&self) -> Arc<dyn Transport> {
        self.transport.clone()
    }

    /// How many HTTP requests reached a role or the directory (ADR-0020
    /// invariant 4).
    #[cfg(test)]
    pub(crate) fn route_counter(&self) -> RouteCounter {
        self.routes.clone()
    }

    /// The live participant registry, so a test can see who is connected.
    /// Nothing in `main.rs` needs this: the runners already hold their own
    /// clone (ADR-0011 C2), which is why this is test-only rather than
    /// `pub`.
    #[cfg(test)]
    pub(crate) fn participants(&self) -> ParticipantRegistry {
        self.participants.clone()
    }

    /// The TCP addresses this broker is bound to (BI-2, AGE-634). A running
    /// `Broker` always has exactly one: its ephemeral loopback gateway port.
    /// [`LazyBroker`] is what a caller checks *before* one exists.
    pub fn bound_addrs(&self) -> Vec<std::net::SocketAddr> {
        vec![std::net::SocketAddr::from(([127, 0, 0, 1], self.port))]
    }

    /// Stop serving. Workers already spawned are reaped by `LocalRunner`'s
    /// own `Drop` when it is dropped, not by this — nothing here waits for
    /// them (Do item 4: they are reaped as the runner already does). `&self`
    /// rather than consuming: [`PendingBroker::shutdown`] (BI-2, AGE-634)
    /// only ever sees this through a shared reference into its `OnceCell`.
    pub fn shutdown(&self) {
        self.server.abort();
        self.participant_listener.abort();
        chatty_protocol_gateway::participant::unbind(&self.socket);
    }
}

/// The leader's own provider flags, to forward to every worker (ADR-0011
/// C10, Do item 4): a leader started with `--ollama`, `--openai-compat-url`
/// or `--api-key` was configured by those flags and nothing else, and a
/// child that does not get them has no provider at all — in a Harbor
/// sandbox every delegation then fails and looks like "the leader never
/// delegates". A leader configured by settings has none of these set and
/// forwards nothing; the child reads the same config dir.
pub fn provider_flags(
    ollama: Option<&str>,
    openai_compat_url: Option<&str>,
    api_key: Option<&str>,
) -> Vec<String> {
    let mut flags = Vec::new();
    if let Some(url) = ollama {
        flags.push("--ollama".to_string());
        flags.push(url.to_string());
    }
    if let Some(url) = openai_compat_url {
        flags.push("--openai-compat-url".to_string());
        flags.push(url.to_string());
    }
    if let Some(key) = api_key {
        flags.push("--api-key".to_string());
        flags.push(key.to_string());
    }
    flags
}

/// What [`PendingBroker::ensure_started`] runs the first time it is called:
/// [`Broker::start`] (production) or [`Broker::start_at`] (tests, an
/// isolated socket and executable).
type StartFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Broker>> + Send>>;

/// [`Broker::start`]'s parameters, captured once so the broker itself —
/// its participant socket and TCP gateway — starts on the first
/// `list_agents`/`invoke_agent` call instead of at process boot (BI-2,
/// AGE-634). `main.rs` builds one of these whenever `--broker`/`--team` is
/// given, instead of starting the broker right away.
pub struct PendingBroker {
    start: Box<dyn Fn() -> StartFuture + Send + Sync>,
    once: tokio::sync::OnceCell<Broker>,
}

impl PendingBroker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        leader_model: ModelConfig,
        models: Vec<ModelConfig>,
        providers: Vec<ProviderConfig>,
        module_settings: ModuleSettingsModel,
        agents: Vec<AgentSpec>,
        workspace_dir: Option<String>,
        auto_approve: bool,
        provider_flags: Vec<String>,
    ) -> Self {
        let start = move || -> StartFuture {
            let leader_model = leader_model.clone();
            let models = models.clone();
            let providers = providers.clone();
            let module_settings = module_settings.clone();
            let agents = agents.clone();
            let workspace_dir = workspace_dir.clone();
            let provider_flags = provider_flags.clone();
            Box::pin(async move {
                Broker::start(
                    &leader_model,
                    &models,
                    &providers,
                    &module_settings,
                    &agents,
                    workspace_dir,
                    auto_approve,
                    &provider_flags,
                )
                .await
            })
        };
        Self {
            start: Box::new(start),
            once: tokio::sync::OnceCell::new(),
        }
    }

    /// As [`Self::new`], but over [`Broker::start_at`]'s injected socket
    /// path and worker executable, so a test never touches the real runtime
    /// directory or shares a socket with another test in this binary
    /// (they all share a pid, and thus [`socket_path`]).
    #[cfg(test)]
    pub(crate) fn new_at(
        socket: PathBuf,
        executable: PathBuf,
        default_budget: usize,
        specs: Vec<VirtualAgentSpec>,
        workspace_dir: Option<String>,
    ) -> Self {
        let start = move || -> StartFuture {
            let socket = socket.clone();
            let executable = executable.clone();
            let specs = specs.clone();
            let workspace_dir = workspace_dir.clone();
            Box::pin(async move {
                Broker::start_at(socket, executable, default_budget, specs, workspace_dir).await
            })
        };
        Self {
            start: Box::new(start),
            once: tokio::sync::OnceCell::new(),
        }
    }
}

#[async_trait::async_trait]
impl chatty_core::services::lazy_broker::LazyBroker for PendingBroker {
    async fn ensure_started(&self) -> anyhow::Result<String> {
        let broker = self.once.get_or_try_init(|| (self.start)()).await?;
        Ok(format!("http://localhost:{}", broker.port))
    }

    /// The leader reaches its broker directly, not over its own loopback
    /// port (ADR-0020, BI-4).
    async fn transport(&self) -> anyhow::Result<Option<Arc<dyn Transport>>> {
        let broker = self.once.get_or_try_init(|| (self.start)()).await?;
        Ok(Some(broker.transport()))
    }

    fn bound_addrs(&self) -> Vec<std::net::SocketAddr> {
        self.once.get().map(Broker::bound_addrs).unwrap_or_default()
    }

    fn shutdown(&self) {
        if let Some(broker) = self.once.get() {
            broker.shutdown();
        }
    }
}

/// One `LocalRunner` per resolved agent, all metered on one shared budget
/// so two agents on the same model server queue against each other and two
/// on different servers do not (ADR-0011 C6/C10). Identical in shape to
/// chatty-gpui's `broker_runner::local_runners`; the decisions it wraps
/// are `chatty_core::services::virtual_agents`', made once for both.
fn local_runners(
    executable: PathBuf,
    registry: ParticipantRegistry,
    default_budget: usize,
    specs: Vec<VirtualAgentSpec>,
    workspace_dir: Option<String>,
) -> Vec<LocalRunner> {
    let mut budget = EndpointBudget::new(default_budget);
    for (endpoint, limit) in specs.iter().filter_map(|spec| spec.endpoint.clone()) {
        budget = budget.with_endpoint(endpoint, limit);
    }

    specs
        .into_iter()
        .map(|spec| {
            let mut runner = LocalRunner::new(executable.clone(), registry.clone())
                .with_agent_name(spec.name)
                .with_description(spec.description)
                .with_args(spec.args);
            if let Some(root) = workspace_dir.clone() {
                runner = runner
                    .with_workspace_factory(worktree_factory(root, spec.verification.clone()));
            }
            if let Some((endpoint, _)) = spec.endpoint {
                runner = runner.with_endpoint_budget(endpoint, budget.clone());
            }
            runner
        })
        .collect()
}

/// Where the shared participant socket is bound, refusing every
/// registration: the runtime directory when there is one, otherwise the
/// temp directory (mirrors chatty-gpui's `broker_runner::
/// socket_path`), suffixed with this process's pid so two `--broker`
/// leaders on one host never share a socket.
fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("chatty")
        .join(format!("participants-{}.sock", std::process::id()))
}

/// Give each worker its own `git worktree`, commit what it leaves behind,
/// and report what that was (AGE-406). `verification` is the team's command
/// for *this* agent, already `None` for a profile with no shell. Identical
/// to chatty-gpui's `broker_runner::worktree_factory`; both wrap the same
/// `chatty_core::services::worker_tree` logic, which is where all of it but
/// the `WorkerWorkspace` glue lives (AGE-376).
fn worktree_factory(workspace_root: String, verification: Option<String>) -> WorkspaceFactory {
    Arc::new(move |worker: String| {
        let workspace_root = workspace_root.clone();
        let verification = verification.clone();
        Box::pin(async move {
            let Some((cwd, evidence, on_exit)) =
                worker_tree::create_with_commit_hook(&workspace_root, &worker, verification)
                    .await?
            else {
                return Ok(None);
            };
            Ok(Some(WorkerWorkspace {
                cwd,
                evidence: Some(Box::new(move || {
                    Box::pin(async move {
                        evidence().await.map(|found| TaskEvidence {
                            text: found.block(),
                            data: found.json(),
                        })
                    })
                })),
                on_exit,
            }))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::services::lazy_broker::LazyBroker;
    use chatty_core::settings::models::providers_store::ProviderType;
    use chatty_core::tools::LOCAL_AGENT_NAME;

    /// A socket path in its own temp dir: every test in this binary shares
    /// one pid, so [`socket_path`] alone would give them all the same path
    /// (`participant::bind` refuses a second listener on a live one) — and
    /// a unit test must not write into the user's real runtime directory
    /// either way. The `TempDir` must outlive the broker using it.
    fn test_socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a temp dir for the socket");
        let socket = dir.path().join("participants.sock");
        (dir, socket)
    }

    /// The agents `start` would resolve for these settings, with the
    /// default worker binary — what every production `--broker` gets.
    fn default_specs(
        models: &[ModelConfig],
        providers: &[ProviderConfig],
        module_settings: &ModuleSettingsModel,
    ) -> Vec<VirtualAgentSpec> {
        resolve_virtual_agents(models, providers, module_settings, &[], &[])
    }

    /// BI-2, AGE-634: nothing is bound or spawned when a `PendingBroker` is
    /// merely constructed, and the first `list_agents` call is what actually
    /// starts it — exactly one broker, not one per call.
    #[tokio::test]
    async fn broker_starts_on_first_use() {
        let module_settings = ModuleSettingsModel::default();
        let (_dir, socket) = test_socket();
        let pending = PendingBroker::new_at(
            socket,
            worker_executable(),
            module_settings.default_endpoint_budget,
            default_specs(&[], &[], &module_settings),
            None,
        );
        assert!(
            pending.bound_addrs().is_empty(),
            "constructing a PendingBroker must not start anything"
        );

        let broker: Arc<dyn chatty_core::services::lazy_broker::LazyBroker> = Arc::new(pending);
        let list_agents_tool = chatty_core::tools::list_agents_tool::ListAgentsTool::new(vec![])
            .with_lazy_broker(broker.clone())
            .with_local_workers([LOCAL_AGENT_NAME]);

        {
            use rig_agent::tool::{Tool, ToolContext};
            list_agents_tool
                .call(
                    &mut ToolContext::new(),
                    chatty_core::tools::list_agents_tool::ListAgentsToolArgs {},
                )
                .await
                .expect("list_agents succeeds even with nothing registered yet");
        }

        let addrs = broker.bound_addrs();
        assert_eq!(
            addrs.len(),
            1,
            "exactly one broker is bound after the first list_agents call: {addrs:?}"
        );

        // A second call reuses the same broker instead of starting another.
        {
            use rig_agent::tool::{Tool, ToolContext};
            list_agents_tool
                .call(
                    &mut ToolContext::new(),
                    chatty_core::tools::list_agents_tool::ListAgentsToolArgs {},
                )
                .await
                .expect("a second list_agents call succeeds too");
        }
        assert_eq!(
            broker.bound_addrs(),
            addrs,
            "a second call does not start a second broker"
        );
    }

    /// BI-2, AGE-634: a broker attached to a tool is not the same as one
    /// asked for. A call that never reaches for the broker — here,
    /// `invoke_agent` for a name that is neither a local worker nor a
    /// module — must not start it either, so the desktop's "module gateway
    /// off" case (where nothing ever needs the broker) opens no TCP port
    /// just because a `LazyBroker` happens to be configured.
    #[tokio::test]
    async fn lazy_broker_opens_no_new_tcp_port() {
        let module_settings = ModuleSettingsModel::default();
        let (_dir, socket) = test_socket();
        let pending = PendingBroker::new_at(
            socket,
            worker_executable(),
            module_settings.default_endpoint_budget,
            default_specs(&[], &[], &module_settings),
            None,
        );
        let broker: Arc<dyn chatty_core::services::lazy_broker::LazyBroker> = Arc::new(pending);

        let invoke_agent_tool =
            chatty_core::tools::invoke_agent_tool::InvokeAgentTool::new(vec![], vec![], None)
                .with_lazy_broker(broker.clone());
        let outcome = {
            use rig_agent::tool::{Tool, ToolContext};
            invoke_agent_tool
                .call(
                    &mut ToolContext::new(),
                    chatty_core::tools::invoke_agent_tool::InvokeAgentArgs {
                        agent: "nobody-registered".to_string(),
                        prompt: "hi".to_string(),
                        include_trace: false,
                    },
                )
                .await
        };
        assert!(
            outcome.is_err(),
            "an unknown agent name is refused, not delegated"
        );
        assert!(
            broker.bound_addrs().is_empty(),
            "a call that never reaches for the broker must not start it"
        );
    }

    #[tokio::test]
    async fn starts_on_an_ephemeral_port_with_no_workspace_or_models() {
        let module_settings = ModuleSettingsModel::default();
        let (_dir, socket) = test_socket();
        let broker = Broker::start_at(
            socket,
            worker_executable(),
            module_settings.default_endpoint_budget,
            default_specs(&[], &[], &module_settings),
            None,
        )
        .await
        .expect("the broker starts with nothing configured");
        assert_ne!(broker.port, 0, "an ephemeral port was actually assigned");
        broker.shutdown();
    }

    /// Do item 3: the socket is suffixed with this process's pid, so two
    /// `--broker` leaders on one host — two separate processes, each with
    /// their own pid — never share a socket. Asserted directly against
    /// `socket_path()` rather than a started `Broker`: every test in this
    /// binary shares a pid, so binding it here would collide with any other
    /// test that also used the real (non-injected) path.
    #[test]
    fn the_socket_is_suffixed_with_this_processs_pid() {
        let socket = socket_path();
        assert!(
            socket
                .to_string_lossy()
                .contains(&std::process::id().to_string()),
            "socket path {} does not carry this process's pid",
            socket.display()
        );
    }

    #[tokio::test]
    async fn a_configured_model_resolves_an_endpoint_budget() {
        let module_settings = ModuleSettingsModel {
            default_endpoint_budget: 2,
            ..Default::default()
        };
        let mut provider = ProviderConfig::new("p".to_string(), ProviderType::Ollama);
        provider.base_url = Some("http://localhost:11434".to_string());
        let models = vec![ModelConfig::new(
            "m1".to_string(),
            "Model One".to_string(),
            ProviderType::Ollama,
            "vendor/model-one".to_string(),
        )];
        let (_dir, socket) = test_socket();

        let specs = default_specs(&models, &[provider], &module_settings);
        assert_eq!(
            specs[0].endpoint,
            Some(("http://localhost:11434".to_string(), 2))
        );
        // Not asserting on the runner's internals (private to the gateway
        // crate) — this just pins that a configured model does not stop the
        // broker from starting.
        let broker = Broker::start_at(
            socket,
            worker_executable(),
            module_settings.default_endpoint_budget,
            specs,
            None,
        )
        .await
        .expect("the broker starts with a resolvable endpoint");
        broker.shutdown();
    }

    /// Reviewer probe (AGE-376 review): the runner must be reachable with
    /// *no* worker ever having registered — `local-agent` is virtual, so
    /// nothing is in the participant registry until a task arrives, and the
    /// only thing standing behind `/a2a/local-agent/...` at that point is
    /// `state.runners` (`handlers/a2a.rs::module_agent_card`). Replacing
    /// `gateway.with_virtual_agent(...)` with `drop(runner)` makes this
    /// fail with a 404, which is how this was confirmed to actually pin
    /// runner registration rather than passing vacuously.
    #[tokio::test]
    async fn the_runner_serves_local_agents_card_with_no_worker_registered() {
        let module_settings = ModuleSettingsModel::default();
        let (_dir, socket) = test_socket();
        let broker = Broker::start_at(
            socket,
            worker_executable(),
            module_settings.default_endpoint_budget,
            default_specs(&[], &[], &module_settings),
            None,
        )
        .await
        .expect("the broker starts");

        let client = chatty_core::services::http_client::default_client(5);
        let url = format!(
            "http://127.0.0.1:{}/a2a/{}/.well-known/agent.json",
            broker.port, LOCAL_AGENT_NAME
        );
        let response = client
            .get(&url)
            .send()
            .await
            .expect("the gateway answers the card request");
        assert!(
            response.status().is_success(),
            "GET {url} returned {}",
            response.status()
        );
        let card: serde_json::Value = response
            .json()
            .await
            .expect("the response body is the agent card JSON");
        assert_eq!(card["name"], LOCAL_AGENT_NAME);

        broker.shutdown();
    }

    /// AGE-402: two brokers on one repository — a top leader and a
    /// sub-leader started with `--broker` inside its own worktree — each
    /// name their first worker `local-coder-0` from their own counter. Each
    /// gets a tree and a branch of its own. A third broker whose tree
    /// cannot be made fails the delegation rather than running its worker
    /// in the shared tree.
    #[tokio::test]
    async fn two_brokers_naming_the_same_worker_get_their_own_branches() {
        let dir = tempfile::tempdir().expect("a repo dir");
        let git = |args: &[&str], cwd: &std::path::Path| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q"], dir.path());
        git(
            &[
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=T",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
            dir.path(),
        );
        let root = dir.path().to_string_lossy().to_string();

        let top = worktree_factory(root.clone(), None);
        let lead = top("local-lead-0".to_string())
            .await
            .expect("the top broker isolates its sub-leader")
            .expect("the workspace is a repository");
        let nested = worktree_factory(lead.cwd.to_string_lossy().to_string(), None);

        let nested_coder = nested("local-coder-0".to_string())
            .await
            .expect("the sub-leader isolates its coder")
            .expect("its worktree is a repository too");
        let top_coder = top("local-coder-0".to_string())
            .await
            .expect("the top broker isolates its coder")
            .expect("the workspace is a repository");

        assert_ne!(nested_coder.cwd, top_coder.cwd);
        assert!(nested_coder.cwd.ends_with("local-coder-0"));
        assert!(
            top_coder.cwd.ends_with("local-coder-0-2"),
            "the top broker's coder yields the name the nested one took: {}",
            top_coder.cwd.display()
        );

        let refused = top("../escape".to_string()).await;
        assert!(
            refused.is_err(),
            "a tree that cannot be made fails the task; it never falls back to the shared tree"
        );
    }

    /// Do item 4: a flag-configured leader forwards exactly its provider
    /// flags; a settings-configured one forwards nothing.
    #[test]
    fn provider_flags_are_forwarded_verbatim_and_only_when_set() {
        assert!(provider_flags(None, None, None).is_empty());
        assert_eq!(
            provider_flags(Some("http://localhost:11434"), None, None),
            vec!["--ollama", "http://localhost:11434"]
        );
        assert_eq!(
            provider_flags(None, Some("http://x"), Some("k")),
            vec!["--openai-compat-url", "http://x", "--api-key", "k"]
        );
    }
}
