//! Core `ProtocolGateway` implementation — builds the axum router and manages
//! the server lifecycle.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio::sync::oneshot;
use tracing::info;

use chatty_module_registry::ModuleRegistry;
use hive_client::{CreditGuard, HiveRegistryClient, UsageCollector};

use crate::handlers::{a2a, index, mcp, openai};

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
}

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
        };

        Router::new()
            // ── Index ────────────────────────────────────────────────────────
            .route("/", get(index::index))
            // ── Aggregated A2A agent card ────────────────────────────────────
            .route("/.well-known/agent.json", get(a2a::aggregated_agent_card))
            // ── OpenAI-compatible endpoints ──────────────────────────────────
            .route(
                "/v1/{module}/chat/completions",
                post(openai::chat_completions_module),
            )
            .route(
                "/v1/chat/completions",
                post(openai::chat_completions_routed),
            )
            // ── MCP endpoints ────────────────────────────────────────────────
            .route("/mcp/{module}", post(mcp::mcp_jsonrpc))
            .route("/mcp/{module}/sse", get(mcp::mcp_sse))
            // ── A2A endpoints ────────────────────────────────────────────────
            .route(
                "/a2a/{module}/.well-known/agent.json",
                get(a2a::module_agent_card),
            )
            .route("/a2a/{module}", post(a2a::a2a_jsonrpc))
            // ── Shared state ─────────────────────────────────────────────────
            .with_state(state)
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
    }

    /// Return the port this gateway is configured to use.
    pub fn port(&self) -> u16 {
        self.port
    }
}

// ---------------------------------------------------------------------------
// Shared utilities available to handler modules
// ---------------------------------------------------------------------------

/// Generate a short, unique-ish ID string for tasks / completion IDs.
pub(crate) fn new_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut h = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut h);
    format!("{:016x}", h.finish())
}
