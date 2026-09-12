//! Wiring the broker's local runners into the desktop's protocol gateway
//! (ADR-0011 C2 / AGE-301; named virtual agents, C10 / AGE-377).
//!
//! The gateway is started by the module-settings controller. This adds the
//! two things that turn it into a fleet broker: a Unix socket children
//! register on, and the virtual agents — `local-agent`, or the named team
//! `module_settings.virtual_agents` declares — that spawn one child per
//! task.
//!
//! Nothing here decides *what* a worker does — the child is `chatty-tui` in
//! participant mode, running the same session the desktop does, and which
//! model and tools each named agent's children get is
//! `chatty_core::services::virtual_agents`' decision, shared with
//! chatty-tui's `--broker`. The only decisions here are where the socket
//! lives, where a worker runs, and how the per-endpoint budget (ADR-0011
//! C6) is wrapped.

use std::path::PathBuf;
use std::sync::Arc;

use chatty_core::services::virtual_agents::VirtualAgentSpec;
use chatty_core::services::worker_tree;
use chatty_core::tools::worker_executable;
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

/// The runners the gateway publishes, one per resolved virtual agent.
///
/// `workspace_dir` is the conversation's workspace root; each worker gets a
/// `git worktree` under it (ADR-0012). Without one — or when it is not a git
/// repository — workers share the desktop's tree, as they did before AGE-314.
/// Every runner with an endpoint is metered on one shared budget, so two
/// agents on the same model server queue against each other and two on
/// different servers do not (ADR-0011 C6/C10); `default_budget` sizes an
/// endpoint nothing more specific is known about.
pub fn local_runners(
    registry: ParticipantRegistry,
    socket: PathBuf,
    workspace_dir: Option<String>,
    default_budget: usize,
    specs: Vec<VirtualAgentSpec>,
) -> Vec<LocalRunner> {
    let mut budget = EndpointBudget::new(default_budget);
    for (endpoint, limit) in specs.iter().filter_map(|spec| spec.endpoint.clone()) {
        info!(
            endpoint = %endpoint,
            limit,
            "Metering the broker's workers on their model endpoint"
        );
        budget = budget.with_endpoint(endpoint, limit);
    }

    specs
        .into_iter()
        .map(|spec| {
            let mut runner =
                LocalRunner::new(worker_executable(), socket.clone(), registry.clone())
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

/// Give each worker its own `git worktree`, and commit what it leaves behind.
fn worktree_factory(workspace_root: String) -> WorkspaceFactory {
    Arc::new(move |worker: String| {
        let workspace_root = workspace_root.clone();
        Box::pin(async move {
            let Some((cwd, merge_hint, on_exit)) =
                worker_tree::create_with_commit_hook(&workspace_root, &worker).await?
            else {
                return Ok(None);
            };
            Ok(Some(WorkerWorkspace {
                cwd,
                merge_hint: Some(merge_hint),
                on_exit,
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
