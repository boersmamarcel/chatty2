//! `--broker`: a headless, pipe or interactive leader runs its own protocol
//! gateway so it can delegate to its virtual agents — `local-agent`, or
//! the named team `module_settings.virtual_agents` declares (ADR-0011 C10)
//! — the way the desktop's module-settings controller does for the GPUI
//! app (AGE-376).
//!
//! This is the same wiring as chatty-gpui's `broker_runner.rs` — a Unix
//! socket children register on, and one virtual agent per resolved
//! [`VirtualAgentSpec`] that spawns a child per delegated task — minus the
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
use chatty_core::services::virtual_agents::{VirtualAgentSpec, resolve_virtual_agents};
use chatty_core::services::worker_tree;
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::ProviderConfig;
use chatty_core::tools::worker_executable;
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::{
    EndpointBudget, LocalRunner, ParticipantRegistry, WorkerWorkspace, WorkspaceFactory,
};
use chatty_wasm_runtime::{CompletionResponse, LlmProvider, Message, ResourceLimits};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// `--broker` loads no WASM modules of its own, so nothing ever calls
/// `llm::complete()` through this registry; it exists only so
/// `ProtocolGateway::new` has one to serve.
struct NoopProvider;

impl LlmProvider for NoopProvider {
    fn complete(
        &self,
        _model: &str,
        _messages: Vec<Message>,
        _tools: Option<String>,
    ) -> std::result::Result<CompletionResponse, String> {
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
}

impl Broker {
    /// `workspace_dir` and `auto_approve` mirror the leader's own execution
    /// settings, exactly as the desktop passes its conversation's workspace
    /// and approval mode to `broker_runner::local_runners`: a worker gets
    /// its own `git worktree` under the same root, and inherits the same
    /// no-human approval policy. `provider_flags` are the leader's own
    /// `--ollama`/`--openai-compat-url`/`--api-key`, forwarded verbatim
    /// (see [`provider_flags`]).
    pub async fn start(
        models: &[ModelConfig],
        providers: &[ProviderConfig],
        module_settings: &ModuleSettingsModel,
        workspace_dir: Option<String>,
        auto_approve: bool,
        provider_flags: &[String],
    ) -> Result<Self> {
        let mut common_args = Vec::new();
        if auto_approve {
            common_args.push("--auto-approve".to_string());
        }
        common_args.extend(provider_flags.iter().cloned());
        let specs = resolve_virtual_agents(models, providers, module_settings, &common_args);
        Self::start_at(
            socket_path(),
            worker_executable(),
            module_settings.default_endpoint_budget,
            specs,
            workspace_dir,
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
    /// spawn a stand-in binary that records its argv.
    pub(crate) async fn start_at(
        socket: PathBuf,
        executable: PathBuf,
        default_budget: usize,
        specs: Vec<VirtualAgentSpec>,
        workspace_dir: Option<String>,
    ) -> Result<Self> {
        let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let registry = ModuleRegistry::new(provider, ResourceLimits::default())
            .context("failed to build the module registry the broker gateway needs")?;
        let shared = Arc::new(tokio::sync::RwLock::new(registry));
        let mut gateway = ProtocolGateway::new(shared, 0);

        let participants = gateway.participants();
        let listener = chatty_protocol_gateway::participant::bind(&socket)
            .with_context(|| format!("failed to bind participant socket {}", socket.display()))?;
        let participant_listener = tokio::spawn(chatty_protocol_gateway::participant::serve(
            listener,
            participants.clone(),
        ));

        for runner in local_runners(
            executable,
            socket.clone(),
            participants.clone(),
            default_budget,
            specs,
            workspace_dir,
        ) {
            gateway = gateway.with_virtual_agent(Arc::new(runner));
        }

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
        })
    }

    /// The live participant registry, so a test can register a scripted
    /// worker the same way `LocalRunner` would register a real one.
    /// Nothing in `main.rs` needs this: the runners already hold their own
    /// clone (ADR-0011 C2), which is why this is test-only rather than
    /// `pub`.
    #[cfg(test)]
    pub(crate) fn participants(&self) -> ParticipantRegistry {
        self.participants.clone()
    }

    /// Stop serving. Workers already spawned are reaped by `LocalRunner`'s
    /// own `Drop` when it is dropped, not by this — nothing here waits for
    /// them (Do item 4: they are reaped as the runner already does).
    pub fn shutdown(self) {
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

/// One `LocalRunner` per resolved agent, all metered on one shared budget
/// so two agents on the same model server queue against each other and two
/// on different servers do not (ADR-0011 C6/C10). Identical in shape to
/// chatty-gpui's `broker_runner::local_runners`; the decisions it wraps
/// are `chatty_core::services::virtual_agents`', made once for both.
fn local_runners(
    executable: PathBuf,
    socket: PathBuf,
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
            let mut runner = LocalRunner::new(executable.clone(), socket.clone(), registry.clone())
                .with_agent_name(spec.name)
                .with_description(spec.description)
                .with_args(spec.args);
            if let Some(root) = workspace_dir.clone() {
                runner = runner.with_workspace_factory(worktree_factory(root));
            }
            if let Some((endpoint, _)) = spec.endpoint {
                runner = runner.with_endpoint_budget(endpoint, budget.clone());
            }
            runner
        })
        .collect()
}

/// Where children register: the runtime directory when there is one,
/// otherwise the temp directory (mirrors chatty-gpui's `broker_runner::
/// socket_path`), suffixed with this process's pid so two `--broker`
/// leaders on one host never share a socket.
fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("chatty")
        .join(format!("participants-{}.sock", std::process::id()))
}

/// Give each worker its own `git worktree`, and commit what it leaves
/// behind. Identical to chatty-gpui's `broker_runner::worktree_factory`;
/// both wrap the same `chatty_core::services::worker_tree` logic, which is
/// where all of it but the `WorkerWorkspace` glue lives (AGE-376).
fn worktree_factory(workspace_root: String) -> WorkspaceFactory {
    Arc::new(move |worker: String| {
        let workspace_root = workspace_root.clone();
        Box::pin(async move {
            let Some((cwd, on_exit)) =
                worker_tree::create_with_commit_hook(&workspace_root, &worker).await?
            else {
                return Ok(None);
            };
            Ok(Some(WorkerWorkspace { cwd, on_exit }))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        resolve_virtual_agents(models, providers, module_settings, &[])
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
