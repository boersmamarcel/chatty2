//! Wiring the broker's local runners into the desktop's protocol gateway
//! (ADR-0011 C2 / AGE-301; named virtual agents, C10 / AGE-377).
//!
//! The gateway is started by the module-settings controller. This adds what
//! turns it into a fleet broker: the virtual agents — the agent specs
//! `module_settings.virtual_agents` names, else `local-agent` and every
//! exposed spec (PL-U5) — that spawn one child
//! per task on a connection the broker makes for it (ADR-0020), and the
//! shared participant socket, which refuses every registration.
//!
//! Nothing here decides *what* a worker does — the child is `chatty-tui` in
//! participant mode, running the same session the desktop does, and which
//! model and tools each named agent's children get is
//! `chatty_core::services::virtual_agents`' decision, shared with
//! chatty-tui's `--broker`. The only decisions here are where the shared
//! socket lives, where a worker runs, and how the per-endpoint budget
//! (ADR-0011 C6) is wrapped.

use std::path::PathBuf;
use std::sync::Arc;

use chatty_core::services::virtual_agents::VirtualAgentSpec;
use chatty_core::services::worker_tree;
use chatty_core::tools::worker_executable;
use chatty_fabric::EndpointBudget;
use chatty_protocol_gateway::participant::{
    LocalRunner, ParticipantRegistry, TaskEvidence, WorkerWorkspace, WorkspaceFactory,
    WorkspaceRequest,
};
use tracing::{info, warn};

/// Where the shared participant socket is bound. Nothing registers on it
/// (ADR-0020); it refuses every connection.
///
/// The owner-only runtime directory the gateway's own socket lives in
/// (`$XDG_RUNTIME_DIR/chatty-run`, else under the cache dir; never the
/// shared temp dir, ADR-0021 § 4).
pub fn socket_path() -> std::io::Result<PathBuf> {
    Ok(chatty_protocol_gateway::access::default_runtime_dir()?.join("participants.sock"))
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
            let mut runner = LocalRunner::new(worker_executable(), registry.clone())
                .with_agent_name(spec.name)
                .with_description(spec.description)
                .with_args(spec.args)
                .with_workspace_root(workspace_dir.clone())
                .with_verification(spec.verification)
                .with_workspace_factory(worktree_factory());
            if let Some((endpoint, _)) = spec.endpoint {
                runner = runner.with_endpoint_budget(endpoint, budget.clone());
            }
            runner
        })
        .collect()
}

/// Give each worker its own `git worktree` under the tree its spawn
/// context names — the root's workspace, or a sub-leader's own tree, on a
/// branch off the sub-leader's (BI-5) — commit what it leaves behind, and
/// report what that was (AGE-406). The verification command is the team's
/// for *this* agent, already `None` for a profile with no shell. Identical
/// to chatty-tui's `participant::broker::worktree_factory`.
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

/// Bind the shared participant socket and serve it, refusing every
/// registration.
///
/// A failure is logged and swallowed: nothing needs the socket to work —
/// workers reach the broker over the connection it makes for each of them.
pub fn serve_socket(socket: std::io::Result<PathBuf>) {
    let socket = match socket {
        Ok(socket) => socket,
        Err(e) => {
            warn!(error = %e, "No directory for the shared participant socket");
            return;
        }
    };
    let socket = &socket;
    match chatty_protocol_gateway::participant::bind(socket) {
        Ok(listener) => {
            tokio::spawn(chatty_protocol_gateway::participant::serve(listener));
        }
        Err(e) => {
            warn!(
                socket = %socket.display(),
                error = %e,
                "Could not bind the shared participant socket"
            );
        }
    }
}
