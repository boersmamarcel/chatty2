//! Core `ProtocolGateway` implementation — builds the axum router and manages
//! the server lifecycle.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tracing::info;

use chatty_module_registry::ModuleRegistry;
use hive_client::{CreditGuard, HiveRegistryClient, UsageCollector};

use crate::handlers::a2a;
use crate::handlers::index;
use crate::handlers::mcp::{self, SseSessions};
use crate::participant::{BrokerCalls, DirectTransport, ParticipantRegistry, VirtualAgent};
use chatty_fabric::{CallPolicy, EdgeLog, Transport, UsagePricer};

// ---------------------------------------------------------------------------
// GatewayState
// ---------------------------------------------------------------------------

/// Shared state for all gateway handlers.
#[derive(Clone)]
pub struct GatewayState {
    pub registry: Arc<RwLock<ModuleRegistry>>,
    pub usage: Option<Arc<UsageCollector>>,
    pub credit_guard: Option<Arc<CreditGuard>>,
    pub hive_client: Option<Arc<HiveRegistryClient>>,
    pub runner_url: Option<String>,
    /// Set of module names that require credits (paid modules).
    /// Credit checks are skipped for modules NOT in this set.
    /// An empty set means no paid modules — all credit checks are skipped.
    pub paid_modules: Arc<HashSet<String>>,
    /// Processes registered over the participant socket (ADR-0011), served
    /// at `/a2a/{name}` and consulted first.
    pub participants: ParticipantRegistry,
    /// The agents that have no participant until a task arrives: each
    /// starts one, routes the task to it, and reaps it (ADR-0011 C2
    /// locally, C8 hosted). Keyed by the name callers address, in name
    /// order so the aggregated card is stable (ADR-0011 C10).
    pub runners: Arc<BTreeMap<String, Arc<dyn VirtualAgent>>>,
    /// Open `GET /mcp/{m}/sse` streams, by session id.
    pub(crate) sse_sessions: SseSessions,
    /// What runs calls over worker connections (BI-4). Held, never read:
    /// it lives as long as the server does, and the registry only holds it
    /// weakly.
    #[allow(dead_code)]
    pub(crate) calls: Arc<BrokerCalls>,
    /// How many HTTP requests reached a role or the directory.
    pub(crate) routes: RouteCounter,
}

/// How many requests the HTTP side served for the broker's roles — the
/// connected participants and virtual agents at `/a2a/{name}` — and for its
/// directory, the aggregated `/.well-known/agent.json` (ADR-0020 invariant
/// 4). Every request this server takes comes from loopback (it binds
/// 127.0.0.1 and refuses anything else), so these are the loopback counts.
/// A worker reaches roles and the directory over its connection, so in a
/// swarm of workers both stay at zero.
#[derive(Clone, Default)]
pub struct RouteCounter {
    roles: Arc<AtomicU64>,
    directory: Arc<AtomicU64>,
}

impl RouteCounter {
    /// Requests for `/a2a/{role}`, its card included.
    pub fn role_requests(&self) -> u64 {
        self.roles.load(Ordering::Relaxed)
    }

    /// Requests for the aggregated agent card.
    pub fn directory_requests(&self) -> u64 {
        self.directory.load(Ordering::Relaxed)
    }

    pub(crate) fn count_role(&self) {
        self.roles.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count_directory(&self) {
        self.directory.fetch_add(1, Ordering::Relaxed);
    }
}

/// The largest request body any route reads (10 MiB). A bigger one is a
/// 413 before the handler runs.
pub const MAX_REQUEST_BYTES: usize = 10 * 1024 * 1024;

// ---------------------------------------------------------------------------
// ProtocolGateway
// ---------------------------------------------------------------------------

/// A single HTTP server that exposes all loaded modules through OpenAI, MCP,
/// and A2A protocols simultaneously.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use tokio::sync::RwLock;
/// use chatty_module_registry::ModuleRegistry;
/// use chatty_protocol_gateway::ProtocolGateway;
/// use chatty_wasm_runtime::{LlmProvider, ResourceLimits};
///
/// # struct NoopProvider;
/// # impl LlmProvider for NoopProvider {
/// #     fn complete(&self, _: &str, _: Vec<chatty_wasm_runtime::Message>, _: Option<String>)
/// #         -> Result<chatty_wasm_runtime::CompletionResponse, String> { Err("noop".into()) }
/// # }
/// # async fn run() -> anyhow::Result<()> {
/// let provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
/// let registry = ModuleRegistry::new(provider, ResourceLimits::default())?;
/// let shared = Arc::new(RwLock::new(registry));
///
/// let mut gateway = ProtocolGateway::new(shared, 8080);
/// gateway.start().await?;
///
/// // ... later:
/// gateway.shutdown();
/// # Ok(())
/// # }
/// ```
pub struct ProtocolGateway {
    registry: Arc<RwLock<ModuleRegistry>>,
    port: u16,
    shutdown_tx: Option<oneshot::Sender<()>>,
    usage: Option<Arc<UsageCollector>>,
    credit_guard: Option<Arc<CreditGuard>>,
    hive_client: Option<Arc<HiveRegistryClient>>,
    runner_url: Option<String>,
    paid_modules: HashSet<String>,
    participants: ParticipantRegistry,
    /// Where local participants register. `None` disables the socket, which
    /// is the default: only an embedder that wants child processes to reach
    /// it should open one.
    participant_socket: Option<PathBuf>,
    participant_task: Option<tokio::task::JoinHandle<()>>,
    runners: BTreeMap<String, Arc<dyn VirtualAgent>>,
    /// Where each call's edge-log row goes (BI-4); `None` writes none.
    edges: Option<Arc<Mutex<EdgeLog>>>,
    /// The spec rules a node's call is checked against (PL-S2); `None`
    /// checks only the call chain.
    call_policy: Option<Arc<dyn CallPolicy>>,
    /// Prices a callee's reported usage on its edge-log row (DP-3).
    usage_pricer: Option<Arc<dyn UsagePricer>>,
    /// Built on first use, from the virtual agents published by then.
    calls: OnceLock<Arc<BrokerCalls>>,
    routes: RouteCounter,
}

impl ProtocolGateway {
    /// Create a new gateway that will listen on `localhost:{port}`.
    ///
    /// The registry is shared as `Arc<RwLock<ModuleRegistry>>` so callers can
    /// continue to load/unload modules while the server is running.
    pub fn new(registry: Arc<RwLock<ModuleRegistry>>, port: u16) -> Self {
        Self {
            registry,
            port,
            shutdown_tx: None,
            usage: None,
            credit_guard: None,
            hive_client: None,
            runner_url: None,
            paid_modules: HashSet::new(),
            participants: ParticipantRegistry::new(),
            participant_socket: None,
            participant_task: None,
            runners: BTreeMap::new(),
            edges: None,
            call_policy: None,
            usage_pricer: None,
            calls: OnceLock::new(),
            routes: RouteCounter::default(),
        }
    }

    /// Attach a [`UsageCollector`] so that WASM invocations are automatically reported.
    pub fn with_usage_collector(mut self, collector: Arc<UsageCollector>) -> Self {
        self.usage = Some(collector);
        self
    }

    /// Attach a [`CreditGuard`] for pre-invocation credit checks on paid modules.
    pub fn with_credit_guard(mut self, guard: Arc<CreditGuard>) -> Self {
        self.credit_guard = Some(guard);
        self
    }

    /// Attach a [`HiveRegistryClient`] for remote module execution.
    pub fn with_hive_client(mut self, client: Arc<HiveRegistryClient>) -> Self {
        self.hive_client = Some(client);
        self
    }

    /// Set the runner URL for remote module execution.
    pub fn with_runner_url(mut self, url: impl Into<String>) -> Self {
        self.runner_url = Some(url.into());
        self
    }

    /// Specify which modules require credits (paid modules).
    ///
    /// Credit checks are only applied to modules in this set.
    /// Free modules are always allowed regardless of credit balance.
    pub fn with_paid_modules(mut self, modules: HashSet<String>) -> Self {
        self.paid_modules = modules;
        self
    }

    /// Bind the shared participant socket at `path`, which refuses every
    /// registration: a worker's connection is made by the broker that
    /// spawns it (ADR-0020), never by the worker dialling in.
    ///
    /// A stale socket file from a previous run is replaced; a live one is
    /// not (see [`participant::bind`](crate::participant::bind)). The socket
    /// is opened by [`start`](Self::start) and removed by
    /// [`shutdown`](Self::shutdown).
    #[cfg(unix)]
    pub fn with_participant_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.participant_socket = Some(path.into());
        self
    }

    /// Publish a [`VirtualAgent`]: a task addressed to it starts a worker,
    /// routes the task to that worker, and reaps it. On the desktop the
    /// worker is a chatty child ([`LocalRunner`](crate::participant::LocalRunner),
    /// ADR-0011 C2); hosted it is a leased microVM (AGE-307). Either way the
    /// agent must be built on this gateway's participant registry, since that
    /// is where its workers register.
    ///
    /// Additive: each call publishes one more agent under its own name
    /// (ADR-0011 C10 — `local-coder`, `local-reviewer`, …). A second agent
    /// with a name already published replaces the first.
    pub fn with_virtual_agent(mut self, runner: Arc<dyn VirtualAgent>) -> Self {
        self.runners.insert(runner.agent_name().to_string(), runner);
        self
    }

    /// Write one edge-log row per call to `log` (BI-4).
    pub fn with_edge_log(mut self, log: EdgeLog) -> Self {
        self.edges = Some(Arc::new(Mutex::new(log)));
        self
    }

    /// Check every `invoke_agent`, the root's included, against `policy` —
    /// the specs' `delegates_to`, `exposed` and `callers` — before anything
    /// is spawned (PL-S2, AGE-745). Set it before [`calls`](Self::calls) is first asked for.
    pub fn with_call_policy(mut self, policy: Arc<dyn CallPolicy>) -> Self {
        self.call_policy = Some(policy);
        self
    }

    /// Price what each callee reports spending on its edge-log row, or say
    /// `unpriced` (DP-3). Set it before [`calls`](Self::calls) is first
    /// asked for.
    pub fn with_usage_pricer(mut self, pricer: Arc<dyn UsagePricer>) -> Self {
        self.usage_pricer = Some(pricer);
        self
    }

    /// The broker's call path: what runs a worker's calls over its
    /// connection, installed on the participant registry the first time it
    /// is asked for. Publish every virtual agent before this — the call path
    /// reaches the ones published by then.
    pub fn calls(&self) -> Arc<BrokerCalls> {
        self.calls
            .get_or_init(|| {
                let calls = Arc::new(
                    BrokerCalls::new(
                        self.participants.clone(),
                        Arc::new(self.runners.clone()),
                        self.edges.clone(),
                    )
                    .with_policy(self.call_policy.clone())
                    .with_pricer(self.usage_pricer.clone()),
                );
                self.participants.install_calls(&calls);
                calls
            })
            .clone()
    }

    /// The in-process root's handle into this broker: calls run directly on
    /// it, with no socket and no HTTP hop (ADR-0020, BI-4).
    pub fn transport(&self) -> Arc<dyn Transport> {
        Arc::new(DirectTransport::new(self.calls()))
    }

    /// The HTTP side's per-route counts for roles and the directory.
    pub fn route_counter(&self) -> RouteCounter {
        self.routes.clone()
    }

    /// The live participant registry.
    ///
    /// Cloning it is how an embedder inspects who is connected, and how a
    /// runner admits the nodes it makes connections for.
    pub fn participants(&self) -> ParticipantRegistry {
        self.participants.clone()
    }

    /// Build the axum [`Router`] for this gateway.
    ///
    /// Exposed separately from `start` to allow embedding the router into a
    /// larger application or for testing with [`axum::serve`].
    pub fn build_router(&self) -> Router {
        let state = GatewayState {
            registry: Arc::clone(&self.registry),
            usage: self.usage.clone(),
            credit_guard: self.credit_guard.clone(),
            hive_client: self.hive_client.clone(),
            runner_url: self.runner_url.clone(),
            paid_modules: Arc::new(self.paid_modules.clone()),
            participants: self.participants.clone(),
            runners: Arc::new(self.runners.clone()),
            sse_sessions: SseSessions::default(),
            calls: self.calls(),
            routes: self.routes.clone(),
        };

        Router::new()
            // ── Index ────────────────────────────────────────────────────────
            .route("/", get(index::index))
            // ── Aggregated A2A agent card ────────────────────────────────────
            .route("/.well-known/agent.json", get(a2a::aggregated_agent_card))
            // ── MCP endpoints ────────────────────────────────────────────────
            .route("/mcp/{module}", post(mcp::mcp_jsonrpc))
            .route(
                "/mcp/{module}/sse",
                get(mcp::mcp_sse).post(mcp::mcp_sse_message),
            )
            // ── A2A endpoints (participants and virtual agents) ──────────────
            .route("/a2a/{agent}/.well-known/agent.json", get(a2a::agent_card))
            .route("/a2a/{agent}", post(a2a::a2a_jsonrpc))
            // ── Shared state ─────────────────────────────────────────────────
            .with_state(state)
            // ── Every route ──────────────────────────────────────────────────
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
            .layer(axum::middleware::from_fn(crate::loopback::loopback_only))
    }

    /// Start the HTTP server in the background.
    ///
    /// Returns immediately after binding to the port.  Call [`shutdown`] to
    /// stop the server gracefully.
    ///
    /// Returns an error if the port cannot be bound.
    pub async fn start(&mut self) -> Result<()> {
        let addr = format!("127.0.0.1:{}", self.port);
        let listener = TcpListener::bind(&addr)
            .await
            .with_context(|| format!("failed to bind to {}", addr))?;

        info!(addr = %addr, "protocol gateway listening");

        let router = self.build_router();

        #[cfg(unix)]
        if let Some(path) = self.participant_socket.clone() {
            let socket = crate::participant::bind(&path)
                .with_context(|| format!("failed to bind participant socket {}", path.display()))?;
            info!(socket = %path.display(), "participant socket listening");
            self.participant_task = Some(tokio::spawn(crate::participant::serve(socket)));
        }

        let (tx, rx) = oneshot::channel::<()>();
        self.shutdown_tx = Some(tx);

        tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await
                .ok();
        });

        Ok(())
    }

    /// Send the shutdown signal to the running server.
    ///
    /// If the server is not running this is a no-op.
    pub fn shutdown(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Dropping the accept task closes the listener; connected
        // participants then see EOF and deregister themselves.
        if let Some(task) = self.participant_task.take() {
            task.abort();
        }
        #[cfg(unix)]
        if let Some(path) = self.participant_socket.as_ref() {
            crate::participant::unbind(path);
        }
    }

    /// Return the port this gateway is configured to use.
    pub fn port(&self) -> u16 {
        self.port
    }
}

// ---------------------------------------------------------------------------
// Shared utilities available to handler modules
// ---------------------------------------------------------------------------

/// A fresh random id for tasks and tool calls.
pub(crate) fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
