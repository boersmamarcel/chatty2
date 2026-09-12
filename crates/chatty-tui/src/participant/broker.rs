//! `--broker`: a headless, pipe or interactive leader runs its own protocol
//! gateway so it can delegate to `local-agent`, the way the desktop's
//! module-settings controller does for the GPUI app (AGE-376).
//!
//! This is the same wiring as chatty-gpui's `broker_runner.rs` — a Unix
//! socket children register on, and the `local-agent` virtual agent that
//! spawns one per delegated task — minus the WASM module registry the
//! desktop's gateway also serves: `--broker` exists to make `local-agent`
//! reachable, not to load modules, so the registry behind it is empty.
//! Module agents chatty-tui already knows about (`--enable`/manifest
//! discovery) are unaffected; they are a separate path from this gateway.
//!
//! Unlike the desktop, which binds a fixed configured port and one
//! well-known socket path, a headless leader is meant to run many at once
//! (benchmarking a team, CI, a script), so both are ephemeral: the HTTP port
//! is OS-assigned, and the socket path is suffixed with this process's pid.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chatty_core::services::worker_tree;
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::settings::models::models_store::ModelConfig;
use chatty_core::settings::models::providers_store::ProviderConfig;
use chatty_core::tools::{LOCAL_AGENT_NAME, worker_executable};
use chatty_module_registry::ModuleRegistry;
use chatty_protocol_gateway::ProtocolGateway;
use chatty_protocol_gateway::participant::{
    EndpointBudget, LocalRunner, WorkerWorkspace, WorkspaceFactory,
};
// Only named by the `participants` field/accessor, which are `#[cfg(test)]`
// (see `Broker`) — nothing in the production path needs the live registry.
#[cfg(test)]
use chatty_protocol_gateway::participant::ParticipantRegistry;
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
/// pid-suffixed participant socket, and `local-agent` behind it.
pub struct Broker {
    /// The ephemeral port `invoke_agent`/`list_agents` reach it on.
    pub port: u16,
    socket: PathBuf,
    // Only read by the test-only `participants()` accessor below; the
    // runner it was built for already holds its own clone.
    #[cfg(test)]
    participants: ParticipantRegistry,
    server: JoinHandle<()>,
    participant_listener: JoinHandle<()>,
}

impl Broker {
    /// `workspace_dir` and `auto_approve` mirror the leader's own execution
    /// settings, exactly as the desktop passes its conversation's workspace
    /// and approval mode to `broker_runner::local_runner`: a worker gets its
    /// own `git worktree` under the same root, and inherits the same
    /// no-human approval policy.
    pub async fn start(
        models: &[ModelConfig],
        providers: &[ProviderConfig],
        module_settings: &ModuleSettingsModel,
        workspace_dir: Option<String>,
        auto_approve: bool,
    ) -> Result<Self> {
        Self::start_at(
            socket_path(),
            models,
            providers,
            module_settings,
            workspace_dir,
            auto_approve,
        )
        .await
    }

    /// As [`start`](Self::start), but the participant socket path is given
    /// rather than derived from [`socket_path`]. `start` is the real seam
    /// (one process, one pid, one path); this is the seam tests use so they
    /// neither collide with each other — every test in one binary shares a
    /// pid, so [`socket_path`] alone gives them all the same path — nor
    /// write into the user's real runtime directory.
    pub(crate) async fn start_at(
        socket: PathBuf,
        models: &[ModelConfig],
        providers: &[ProviderConfig],
        module_settings: &ModuleSettingsModel,
        workspace_dir: Option<String>,
        auto_approve: bool,
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

        let endpoint = chatty_core::services::worker_endpoint::resolve_worker_endpoint(
            models,
            providers,
            module_settings,
        )
        .map(|(endpoint, limit)| {
            let budget = EndpointBudget::new(module_settings.default_endpoint_budget)
                .with_endpoint(&endpoint, limit);
            (endpoint, budget)
        });

        let mut args: Vec<String> = Vec::new();
        if auto_approve {
            args.push("--auto-approve".to_string());
        }
        let mut runner =
            LocalRunner::new(worker_executable(), socket.clone(), participants.clone())
                .with_agent_name(LOCAL_AGENT_NAME)
                .with_args(args);
        if let Some(root) = workspace_dir {
            runner = runner.with_workspace_factory(worktree_factory(root));
        }
        if let Some((endpoint, budget)) = endpoint {
            runner = runner.with_endpoint_budget(endpoint, budget);
        }
        gateway = gateway.with_virtual_agent(Arc::new(runner));

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
    /// `local-agent` the same way `LocalRunner` would register a real one.
    /// Nothing in `main.rs` needs this: the runner already holds its own
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

    #[tokio::test]
    async fn starts_on_an_ephemeral_port_with_no_workspace_or_models() {
        let module_settings = ModuleSettingsModel::default();
        let (_dir, socket) = test_socket();
        let broker = Broker::start_at(socket, &[], &[], &module_settings, None, false)
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

        // Not asserting on the runner's internals (private to the gateway
        // crate) — this just pins that a configured model does not stop the
        // broker from starting.
        let broker = Broker::start_at(socket, &models, &[provider], &module_settings, None, false)
            .await
            .expect("the broker starts with a resolvable endpoint");
        broker.shutdown();
    }

    /// Reviewer probe (AGE-376 review): the runner must be reachable with
    /// *no* worker ever having registered — `local-agent` is virtual, so
    /// nothing is in the participant registry until a task arrives, and the
    /// only thing standing behind `/a2a/local-agent/...` at that point is
    /// `state.runner` (`handlers/a2a.rs::module_agent_card`). Replacing
    /// `gateway.with_virtual_agent(...)` with `drop(runner)` makes this
    /// fail with a 404, which is how this was confirmed to actually pin
    /// runner registration rather than passing vacuously.
    #[tokio::test]
    async fn the_runner_serves_local_agents_card_with_no_worker_registered() {
        let module_settings = ModuleSettingsModel::default();
        let (_dir, socket) = test_socket();
        let broker = Broker::start_at(socket, &[], &[], &module_settings, None, false)
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
}
