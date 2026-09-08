//! Wiring the broker's local runner into the desktop's protocol gateway
//! (ADR-0011 C2 / AGE-301).
//!
//! The gateway is started by the module-settings controller. This adds the
//! two things that turn it into a fleet broker: a Unix socket children
//! register on, and the `local-agent` virtual agent that spawns one per task.
//!
//! Nothing here decides *what* a worker does — the child is `chatty-tui` in
//! participant mode, running the same session the desktop does. The only
//! decisions are where the socket lives, where a worker runs, and how many
//! of them may run at once against one model server (ADR-0011 C6).

use std::path::PathBuf;
use std::sync::Arc;

use chatty_core::services::worker_tree;
use chatty_core::settings::models::ModuleSettingsModel;
use chatty_core::settings::models::models_store::ModelsModel;
use chatty_core::settings::models::providers_store::ProviderModel;
use chatty_core::tools::{LOCAL_AGENT_NAME, worker_executable};
use chatty_protocol_gateway::participant::{
    EndpointBudget, LocalRunner, ParticipantRegistry, WorkerWorkspace, WorkspaceFactory,
};
use tracing::{info, warn};

/// Where children register.
///
/// The runtime directory when there is one (`/run/user/<uid>`, cleaned up on
/// logout), otherwise the temp directory. Unix socket paths are limited to
/// about 100 bytes, so this stays short deliberately — a path under the
/// user's data directory would overflow it on some systems.
pub fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("chatty")
        .join("participants.sock")
}

/// The model endpoint every worker will share, and the budget that meters it
/// (ADR-0011 C6).
///
/// A worker resolves its own model exactly as `chatty-tui` does when it is
/// spawned without `--model`: the first model in the roster. So the endpoint
/// to meter is that model's provider's, and a desktop with no model
/// configured has nothing to meter — the delegation would fail in the child
/// anyway.
///
/// The size is the provider's own parallel-request setting where it is
/// known, an explicit per-endpoint override where there is one, and
/// otherwise the configured default of one.
pub fn worker_endpoint(
    models: &ModelsModel,
    providers: &ProviderModel,
    module_settings: &ModuleSettingsModel,
) -> Option<(String, EndpointBudget)> {
    let model = models.models().first()?;
    let provider = providers
        .providers()
        .iter()
        .find(|p| p.provider_type == model.provider_type)?;

    let endpoint = provider.endpoint_key();
    let limit = module_settings.endpoint_budget(&endpoint, provider.parallel_requests());
    info!(
        endpoint = %endpoint,
        limit,
        "Metering the broker's workers on their model endpoint"
    );

    let budget = EndpointBudget::new(module_settings.default_endpoint_budget)
        .with_endpoint(&endpoint, limit);
    Some((endpoint, budget))
}

/// The runner the gateway publishes as `local-agent`.
///
/// `workspace_dir` is the conversation's workspace root; each worker gets a
/// `git worktree` under it (ADR-0012). Without one — or when it is not a git
/// repository — workers share the desktop's tree, as they did before AGE-314.
/// `endpoint` is [`worker_endpoint`]'s answer: without one the runner spawns
/// as many workers at once as it is asked to.
pub fn local_runner(
    registry: ParticipantRegistry,
    socket: PathBuf,
    workspace_dir: Option<String>,
    auto_approve: bool,
    endpoint: Option<(String, EndpointBudget)>,
) -> LocalRunner {
    let mut args: Vec<String> = Vec::new();
    if auto_approve {
        args.push("--auto-approve".to_string());
    }

    let mut runner = LocalRunner::new(worker_executable(), socket, registry)
        .with_agent_name(LOCAL_AGENT_NAME)
        .with_args(args);

    if let Some(root) = workspace_dir {
        runner = runner.with_workspace_factory(worktree_factory(root));
    }
    if let Some((endpoint, budget)) = endpoint {
        runner = runner.with_endpoint_budget(endpoint, budget);
    }
    runner
}

/// Give each worker its own `git worktree`, and commit what it leaves behind.
fn worktree_factory(workspace_root: String) -> WorkspaceFactory {
    Arc::new(move |worker: String| {
        let workspace_root = workspace_root.clone();
        Box::pin(async move {
            let Some(tree) = worker_tree::create(&workspace_root, &worker).await? else {
                return Ok(None);
            };
            let cwd = tree.path.clone();
            Ok(Some(WorkerWorkspace {
                cwd,
                // `on_exit` runs from the runner's `Drop`, which cannot await;
                // committing is a `git` subprocess, so it is spawned. The tree
                // is the worker's alone, so nothing races this.
                on_exit: Box::new(move |_succeeded| {
                    tokio::spawn(async move {
                        // Committed whether the worker succeeded or not: a
                        // failed worker's partial edits are still the only
                        // copy that exists.
                        worker_tree::commit(&tree, "delegated task").await;
                    });
                }),
            }))
        })
    })
}

/// Open the participant socket and serve it, returning whether it came up.
///
/// A failure is logged and swallowed: the gateway's HTTP surface is what
/// modules need and it works without a socket. Only delegation to
/// `local-agent` is lost, and `invoke_agent` reports that itself when the
/// call fails.
pub fn serve_socket(registry: ParticipantRegistry, socket: &PathBuf) -> bool {
    match chatty_protocol_gateway::participant::bind(socket) {
        Ok(listener) => {
            tokio::spawn(chatty_protocol_gateway::participant::serve(
                listener, registry,
            ));
            true
        }
        Err(e) => {
            warn!(
                socket = %socket.display(),
                error = %e,
                "No local participant socket; delegation to a local worker is unavailable"
            );
            false
        }
    }
}
