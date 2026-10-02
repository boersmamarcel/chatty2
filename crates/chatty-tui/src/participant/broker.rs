//! `--broker`: a headless, pipe or interactive leader runs its own protocol
//! gateway so it can delegate to its virtual agents — `local-agent`, or
//! the agent specs `module_settings.virtual_agents` names (ADR-0011 C10)
//! — the way the desktop's module-settings controller does for the GPUI
//! app (AGE-376).
//!
//! Only a root process starts one (ADR-0020, BI-5). A sub-leader — a worker
//! whose spec delegates in turn — has no broker of its own: its calls go
//! over the connection the root's broker made for it, and the root spawns
//! its workers with a spawn context the root derives from the sub-leader's
//! own (its tree, its branch, its roster), so they branch from the
//! sub-leader's branch as they did when it ran a broker itself.
//!
//! This is the same wiring as chatty-gpui's `broker_runner.rs` — one
//! virtual agent per resolved [`VirtualAgentSpec`] that spawns a child per
//! delegated task on a connection the broker makes for it, and the shared
//! participant socket that refuses every registration (ADR-0020) — minus the
//! WASM module registry the desktop's gateway also serves: `--broker`
//! exists to make the workers reachable, not to load modules, so the
//! registry behind it is empty. A plugin is never an agent (PL-U5): a
//! worker loads the plugins its spec lists itself.
//!
//! Unlike the desktop, which binds one well-known socket path, a headless
//! leader is meant to run many at once (benchmarking a team, CI, a script),
//! so its sockets are suffixed with this process's pid. Both — the shared
//! participant socket and the gateway's own, which requires its per-launch
//! token on every route (ADR-0021 § 4, EN-0d) — live in the owner-only
//! runtime directory; there is no TCP listener.
//!
//! A leader configured by flags (`--ollama`, `--openai-compat-url`,
//! `--api-key`) has no config dir a child could read, so those flags are
//! forwarded to every worker (`common_args`); a settings-configured leader
//! forwards nothing, since the child reads the same files.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chatty_core::agent_spec::AgentSpec;
use chatty_core::models::token_usage::PriceBook;
use chatty_core::services::delegation_policy::SpecPolicy;
use chatty_core::services::plugin_llm::PluginLlmProvider;
use chatty_core::services::virtual_agents::{VirtualAgentSpec, resolve_virtual_agents};
use chatty_core::services::worker_tree;
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::ProviderConfig;
use chatty_core::tools::worker_executable;
use chatty_fabric::{EdgeLog, EndpointBudget, HandoffContract, Transport};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::access::{PrivateDir, SocketServer};
use chatty_protocol_gateway::participant::{
    LocalRunner, ParticipantRegistry, TaskEvidence, WorkerWorkspace, WorkspaceFactory,
    WorkspaceRequest,
};
#[cfg(test)]
use chatty_protocol_gateway::{GatewayToken, RouteCounter};
use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
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

/// The gateway a `--broker` leader runs: its token-guarded socket, a
/// pid-suffixed participant socket, and the virtual agents behind it.
pub struct Broker {
    socket: PathBuf,
    /// The gateway's own listener. Nothing in this process uses it — the
    /// leader's calls go over [`transport`](Self::transport) — but every
    /// listener serving the router requires the launch token (EN-0d).
    gateway: SocketServer,
    #[cfg(test)]
    token: GatewayToken,
    // Only read by the test-only `participants()` accessor below; the
    // runners it was built for already hold their own clone.
    #[cfg(test)]
    participants: ParticipantRegistry,
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
    /// loaded (an empty team roster is the one default worker, as
    /// `resolve_virtual_agents` resolves it). `leader_model` is the model
    /// this leader runs: what a module's `llm::complete("")` is served by
    /// (PL-H2, AGE-605). `handoffs` is the team's contract per role (TD-2,
    /// AGE-693): each role's runner hands its workers theirs. `root` is
    /// the spec the leader runs as — a `--team` leader, an `--agent <spec>`
    /// root — whose `delegates_to` its own calls are checked against, or
    /// `None` for a plain root, which may call anyone (AGE-745).
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        root: Option<AgentSpec>,
        leader_model: &ModelConfig,
        models: &[ModelConfig],
        providers: &[ProviderConfig],
        module_settings: &ModuleSettingsModel,
        agents: &[AgentSpec],
        handoffs: &BTreeMap<String, HandoffContract>,
        workspace_dir: Option<String>,
        auto_approve: bool,
        provider_flags: &[String],
    ) -> Result<Self> {
        let mut common_args = Vec::new();
        if auto_approve {
            common_args.push("--auto-approve".to_string());
        }
        common_args.extend(provider_flags.iter().cloned());
        let mut specs = resolve_virtual_agents(
            models,
            providers,
            module_settings,
            agents,
            &common_args,
            workspace_dir.as_deref().map(std::path::Path::new),
        );
        for spec in &mut specs {
            spec.handoff = handoffs.get(&spec.name).cloned();
        }
        let llm: Arc<dyn LlmProvider> = Arc::new(PluginLlmProvider::new(
            leader_model.clone(),
            models.to_vec(),
            providers.to_vec(),
            tokio::runtime::Handle::current(),
        ));
        // Each task row prices what its callee reported spending, at the
        // models the lines name (DP-3, AGE-682).
        let mut prices = PriceBook::default();
        for model in models {
            if let Some(pricing) = model.token_pricing() {
                prices.insert(model.model_ref(), pricing);
            }
        }
        Self::serve(
            llm,
            root,
            socket_path()?,
            worker_executable(),
            module_settings.default_endpoint_budget,
            specs,
            workspace_dir,
            dirs::data_dir(),
            Some(prices),
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
        Self::start_priced_at(
            None,
            socket,
            executable,
            default_budget,
            specs,
            workspace_dir,
            None,
        )
        .await
    }

    /// As [`start_at`](Self::start_at), with the root running as `root`
    /// (see [`start`](Self::start)) and each task row's reported usage
    /// priced with `prices` (DP-3).
    #[cfg(test)]
    pub(crate) async fn start_priced_at(
        root: Option<AgentSpec>,
        socket: PathBuf,
        executable: PathBuf,
        default_budget: usize,
        specs: Vec<VirtualAgentSpec>,
        workspace_dir: Option<String>,
        prices: Option<PriceBook>,
    ) -> Result<Self> {
        let data_dir = socket.parent().map(std::path::Path::to_path_buf);
        Self::serve(
            Arc::new(NoopProvider),
            root,
            socket,
            executable,
            default_budget,
            specs,
            workspace_dir,
            data_dir,
            prices,
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
    /// edge log under `data_dir`, when there is one, with its callee's
    /// reported usage priced by `prices`, when there are some.
    #[allow(clippy::too_many_arguments)]
    async fn serve(
        provider: Arc<dyn LlmProvider>,
        root: Option<AgentSpec>,
        socket: PathBuf,
        executable: PathBuf,
        default_budget: usize,
        specs: Vec<VirtualAgentSpec>,
        workspace_dir: Option<String>,
        data_dir: Option<PathBuf>,
        prices: Option<PriceBook>,
    ) -> Result<Self> {
        let registry = ModuleRegistry::new(provider, ResourceLimits::default())
            .context("failed to build the module registry the broker gateway needs")?;
        let shared = Arc::new(tokio::sync::RwLock::new(registry));
        // A call is checked against the roster's specs before anything is
        // spawned (PL-S2), the root's against the spec it runs as (AGE-745).
        let mut gateway = ProtocolGateway::new(shared)
            .with_call_policy(Arc::new(SpecPolicy::for_agents(&specs).with_root(root)));
        if let Some(prices) = prices {
            gateway = gateway.with_usage_pricer(Arc::new(prices));
        }

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

        // The gateway's socket goes beside the participant socket, in the
        // directory `participant::bind` already checked is this user's
        // alone, pid-suffixed like it.
        let dir = socket
            .parent()
            .context("the participant socket has a directory")?;
        let dir = PrivateDir::open(dir).context("the broker's socket directory")?;
        let server = SocketServer::bind(
            &dir,
            &format!("gateway-{}", std::process::id()),
            gateway.build_router(),
            gateway.token(),
        )
        .context("failed to serve the broker gateway")?;

        Ok(Self {
            socket,
            gateway: server,
            #[cfg(test)]
            token: gateway.token().clone(),
            #[cfg(test)]
            participants,
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

    /// The sockets this broker serves on (BI-2, AGE-634): the gateway's
    /// and the shared participant socket. [`LazyBroker`] is what a caller
    /// checks *before* they exist.
    pub fn bound_sockets(&self) -> Vec<PathBuf> {
        vec![
            self.gateway.socket_path().to_path_buf(),
            self.socket.clone(),
        ]
    }

    /// The gateway's socket and token file.
    #[cfg(test)]
    pub(crate) fn gateway(&self) -> &SocketServer {
        &self.gateway
    }

    /// The gateway's launch token.
    #[cfg(test)]
    pub(crate) fn token(&self) -> &GatewayToken {
        &self.token
    }

    /// A raw HTTP GET over the gateway's socket, with `token` as its bearer
    /// when given: the whole response, status line first.
    #[cfg(test)]
    pub(crate) async fn http_get(&self, path: &str, token: Option<&str>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::UnixStream::connect(self.gateway.socket_path())
            .await
            .expect("the gateway is listening");
        let auth = token
            .map(|token| format!("Authorization: Bearer {token}\r\n"))
            .unwrap_or_default();
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    /// Stop serving. Workers already spawned are reaped by `LocalRunner`'s
    /// own `Drop` when it is dropped, not by this — nothing here waits for
    /// them (Do item 4: they are reaped as the runner already does). `&self`
    /// rather than consuming: [`PendingBroker::shutdown`] (BI-2, AGE-634)
    /// only ever sees this through a shared reference into its `OnceCell`.
    pub fn shutdown(&self) {
        self.gateway.stop();
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
/// forwards nothing; the child reads the same config dir. `--think` rides
/// along too (AGE-808): it sets the thinking switch on the leader's model
/// only, so a team run with `--think false` otherwise had every worker
/// thinking.
pub fn provider_flags(
    ollama: Option<&str>,
    openai_compat_url: Option<&str>,
    api_key: Option<&str>,
    think: Option<bool>,
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
    if let Some(think) = think {
        flags.push("--think".to_string());
        flags.push(think.to_string());
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
        root: Option<AgentSpec>,
        leader_model: ModelConfig,
        models: Vec<ModelConfig>,
        providers: Vec<ProviderConfig>,
        module_settings: ModuleSettingsModel,
        agents: Vec<AgentSpec>,
        handoffs: BTreeMap<String, HandoffContract>,
        workspace_dir: Option<String>,
        auto_approve: bool,
        provider_flags: Vec<String>,
    ) -> Self {
        let start = move || -> StartFuture {
            let root = root.clone();
            let leader_model = leader_model.clone();
            let models = models.clone();
            let providers = providers.clone();
            let module_settings = module_settings.clone();
            let agents = agents.clone();
            let handoffs = handoffs.clone();
            let workspace_dir = workspace_dir.clone();
            let provider_flags = provider_flags.clone();
            Box::pin(async move {
                Broker::start(
                    root,
                    &leader_model,
                    &models,
                    &providers,
                    &module_settings,
                    &agents,
                    &handoffs,
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
    /// The leader reaches its broker directly, not over its own gateway
    /// socket (ADR-0020, BI-4).
    async fn transport(&self) -> anyhow::Result<Option<Arc<dyn Transport>>> {
        let broker = self.once.get_or_try_init(|| (self.start)()).await?;
        Ok(Some(broker.transport()))
    }

    fn bound_sockets(&self) -> Vec<PathBuf> {
        self.once
            .get()
            .map(Broker::bound_sockets)
            .unwrap_or_default()
    }

    fn shutdown(&self) {
        if let Some(broker) = self.once.get() {
            broker.shutdown();
        }
    }

    /// The root's messages come from its broker's direct handle; a broker
    /// that has not started yet has none (TM-2).
    fn take_run_messages(&self) -> Vec<String> {
        self.once
            .get()
            .map(|broker| broker.transport().take_run_messages())
            .unwrap_or_default()
    }

    /// `/stop <agent>` stops one run through the direct handle (TB-7); a
    /// broker that has not started runs nothing.
    fn cancel(&self, node: &str) -> anyhow::Result<()> {
        let Some(broker) = self.once.get() else {
            anyhow::bail!("the broker is not running, so nothing is named '{node}'");
        };
        broker.transport().cancel(node).map_err(anyhow::Error::from)
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
                .with_args(spec.args)
                .with_workspace_root(workspace_dir.clone())
                .with_verification(spec.verification)
                .with_handoff(spec.handoff)
                .with_workspace_factory(worktree_factory());
            if let Some((endpoint, _)) = spec.endpoint {
                runner = runner.with_endpoint_budget(endpoint, budget.clone());
            }
            runner
        })
        .collect()
}

/// Where the shared participant socket is bound, refusing every
/// registration: the owner-only runtime directory (mirrors chatty-gpui's
/// `broker_runner::socket_path`), suffixed with this process's pid so two
/// `--broker` leaders on one host never share a socket. The gateway's own
/// socket goes beside it.
fn socket_path() -> Result<PathBuf> {
    Ok(chatty_protocol_gateway::access::default_runtime_dir()?
        .join(format!("participants-{}.sock", std::process::id())))
}

/// Give each worker its own `git worktree` under the tree its spawn
/// context names — the root's workspace, or a sub-leader's own tree, on a
/// branch off the sub-leader's (BI-5) — commit what it leaves behind, and
/// report what that was (AGE-406). The verification command is the team's
/// for *this* agent, already `None` for a profile with no shell. Identical
/// to chatty-gpui's `broker_runner::worktree_factory`; both wrap the same
/// `chatty_core::services::worker_tree` logic, which is where all of it but
/// the `WorkerWorkspace` glue lives (AGE-376).
fn worktree_factory() -> WorkspaceFactory {
    Arc::new(|request: WorkspaceRequest| {
        Box::pin(async move {
            let Some(root) = request.workspace_root else {
                return Ok(None);
            };
            let Some(worker_tree::IsolatedWorker {
                cwd,
                branch,
                evidence,
                on_exit,
            }) = worker_tree::create_with_commit_hook(
                &root,
                &request.worker,
                request.base_branch.as_deref(),
                request.verification,
            )
            .await?
            else {
                return Ok(None);
            };
            Ok(Some(WorkerWorkspace {
                cwd,
                branch: Some(branch),
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
        let socket = dir.path().join("run").join("participants.sock");
        (dir, socket)
    }

    /// The agents `start` would resolve for these settings, with the
    /// default worker binary — what every production `--broker` gets.
    fn default_specs(
        models: &[ModelConfig],
        providers: &[ProviderConfig],
        module_settings: &ModuleSettingsModel,
    ) -> Vec<VirtualAgentSpec> {
        resolve_virtual_agents(models, providers, module_settings, &[], &[], None)
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
            pending.bound_sockets().is_empty(),
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

        let addrs = broker.bound_sockets();
        assert_eq!(
            addrs.len(),
            2,
            "exactly one broker (its gateway and participant sockets) is bound after the \
             first list_agents call: {addrs:?}"
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
            broker.bound_sockets(),
            addrs,
            "a second call does not start a second broker"
        );
    }

    /// BI-2, AGE-634: a broker attached to a tool is not the same as one
    /// asked for. A call that never reaches for the broker — here,
    /// `invoke_agent` for a name that is neither a local worker nor a
    /// module — must not start it either, so the desktop's "module gateway
    /// off" case (where nothing ever needs the broker) binds nothing just
    /// because a `LazyBroker` happens to be configured.
    #[tokio::test]
    async fn lazy_broker_binds_nothing_until_used() {
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

        let invoke_agent_tool = chatty_core::tools::invoke_agent_tool::InvokeAgentTool::new(vec![])
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
            broker.bound_sockets().is_empty(),
            "a call that never reaches for the broker must not start it"
        );
    }

    #[tokio::test]
    async fn starts_with_no_workspace_or_models() {
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
        for socket in broker.bound_sockets() {
            assert!(socket.exists(), "{} is bound", socket.display());
        }
        broker.shutdown();
        assert!(
            !broker.gateway().socket_path().exists(),
            "shutdown removes the gateway socket"
        );
    }

    /// Do item 3: the socket is suffixed with this process's pid, so two
    /// `--broker` leaders on one host — two separate processes, each with
    /// their own pid — never share a socket. Asserted directly against
    /// `socket_path()` rather than a started `Broker`: every test in this
    /// binary shares a pid, so binding it here would collide with any other
    /// test that also used the real (non-injected) path.
    #[test]
    fn the_socket_is_suffixed_with_this_processs_pid() {
        let socket = socket_path().expect("a runtime directory");
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
    /// only thing standing behind it is `state.runners`. Reached over the
    /// root's direct handle now (ADR-0020, BI-7), never over loopback:
    /// replacing `gateway.with_virtual_agent(...)` with `drop(runner)`
    /// makes `local-agent` absent from the directory, which is how this was
    /// confirmed to actually pin runner registration rather than passing
    /// vacuously.
    #[tokio::test]
    async fn the_runner_serves_local_agents_card_with_no_worker_registered() {
        use chatty_fabric::{CallEvent, CallRequest};
        use futures::StreamExt;

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

        let mut stream = broker
            .transport()
            .call(CallRequest::ListAgents)
            .await
            .expect("the root's direct handle answers list_agents");
        let directory = loop {
            match stream.next().await.expect("a result event") {
                Ok(CallEvent::Result(directory)) => break directory,
                Ok(_) => continue,
                Err(error) => panic!("list_agents failed: {error}"),
            }
        };
        let names: Vec<&str> = directory
            .as_array()
            .expect("the directory is a JSON array")
            .iter()
            .filter_map(|agent| agent["name"].as_str())
            .collect();
        assert!(
            names.contains(&LOCAL_AGENT_NAME),
            "local-agent is not in the directory: {names:?}"
        );

        broker.shutdown();
    }

    /// EN-0d (ADR-0021 § 4): the `--broker` leader's own gateway listener
    /// serves `build_router()`, so it refuses every route without the
    /// launch token, and lives in the owner-only directory with the token
    /// in a `0600` file beside it — no TCP port.
    #[tokio::test]
    async fn broker_leader_listener_requires_token() {
        use std::os::unix::fs::PermissionsExt;

        let module_settings = ModuleSettingsModel::default();
        let (dir, socket) = test_socket();
        let broker = Broker::start_at(
            socket,
            worker_executable(),
            module_settings.default_endpoint_budget,
            default_specs(&[], &[], &module_settings),
            None,
        )
        .await
        .expect("the broker starts");

        let gateway = broker.gateway();
        let run = dir.path().join("run");
        assert_eq!(gateway.socket_path().parent(), Some(run.as_path()));
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&run), 0o700);
        assert_eq!(mode(gateway.token_path()), 0o600);
        let token = std::fs::read_to_string(gateway.token_path()).unwrap();
        assert_eq!(token, broker.token().as_str());

        for path in [
            "/",
            "/.well-known/agent.json",
            "/a2a/local-agent/.well-known/agent.json",
            "/mcp/anything/sse",
        ] {
            for wrong in [None, Some("not-the-token")] {
                let response = broker.http_get(path, wrong).await;
                assert!(
                    response.starts_with("HTTP/1.1 401"),
                    "GET {path} with {wrong:?}: {response}"
                );
            }
        }
        let response = broker.http_get("/", Some(&token)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");

        broker.shutdown();
        assert!(
            !gateway.token_path().exists(),
            "shutdown removes the token file"
        );
    }

    /// EN-0d: the token reaches nobody through argv — not this process's,
    /// not a worker's. A delegated worker (the stand-in, which records its
    /// argv) is spawned, and neither command line carries it.
    #[tokio::test]
    async fn token_not_in_process_args() {
        use chatty_core::session::SessionEvent;
        use chatty_fabric::{CallEvent, CallRequest};
        use futures::StreamExt;

        let dir = tempfile::tempdir().expect("a temp dir");
        let executable = super::super::stand_in::scripted_worker_binary(
            dir.path(),
            &[SessionEvent::Text("done".to_string())],
        );
        let module_settings = ModuleSettingsModel::default();
        let broker = Broker::start_at(
            dir.path().join("run").join("participants.sock"),
            executable,
            module_settings.default_endpoint_budget,
            default_specs(&[], &[], &module_settings),
            None,
        )
        .await
        .expect("the broker starts");
        let token = broker.token().as_str().to_string();

        let mut stream = broker
            .transport()
            .call(CallRequest::InvokeAgent(
                serde_json::from_value(serde_json::json!({
                    "agent": LOCAL_AGENT_NAME,
                    "prompt": "hi",
                }))
                .expect("invoke params"),
            ))
            .await
            .expect("the call starts");
        while let Some(event) = stream.next().await {
            if matches!(event, Ok(CallEvent::Result(_)) | Err(_)) {
                break;
            }
        }

        let argv = super::super::stand_in::recorded_argv(dir.path(), 1).await;
        assert!(!argv.is_empty());
        for line in &argv {
            assert!(!line.contains(&token), "a worker's argv carries the token");
        }
        let own = std::fs::read("/proc/self/cmdline").unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&own).contains(&token),
            "this process's argv carries the token"
        );
        broker.shutdown();
    }

    /// Do item 4: a flag-configured leader forwards exactly its provider
    /// flags; a settings-configured one forwards nothing.
    #[test]
    fn provider_flags_are_forwarded_verbatim_and_only_when_set() {
        assert!(provider_flags(None, None, None, None).is_empty());
        assert_eq!(
            provider_flags(Some("http://localhost:11434"), None, None, None),
            vec!["--ollama", "http://localhost:11434"]
        );
        assert_eq!(
            provider_flags(None, Some("http://x"), Some("k"), Some(false)),
            vec![
                "--openai-compat-url",
                "http://x",
                "--api-key",
                "k",
                "--think",
                "false"
            ]
        );
    }
}
