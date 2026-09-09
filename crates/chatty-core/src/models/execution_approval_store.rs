use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use parking_lot::Mutex;

/// Decision for an execution approval request
#[derive(Clone, Debug)]
pub enum ApprovalDecision {
    Approved,
    Denied,
}

/// Notification that an approval request was created
#[derive(Clone, Debug)]
pub struct ApprovalNotification {
    pub id: String,
    pub command: String,
    pub is_sandboxed: bool,
}

/// Notification that an approval was resolved
#[derive(Clone, Debug)]
pub struct ApprovalResolution {
    pub id: String,
    pub approved: bool,
}

/// Inner state behind `PendingApprovals`: the in-flight requests plus the
/// per-turn notifier the frontend installs (AGE-246 / D7) so a tool can
/// announce a new request through the store it was handed, rather than a
/// process-wide global that the next turn — on any conversation — would
/// silently overwrite.
///
/// Each pending request is just the channel to send its decision back to the
/// waiting execution; the id/command/is_sandboxed metadata a UI needs is
/// already carried separately by `ApprovalNotification`, sent at the same
/// time a request is inserted here.
pub struct PendingApprovalsState {
    requests: HashMap<String, oneshot::Sender<ApprovalDecision>>,
    notifier: Option<mpsc::UnboundedSender<ApprovalNotification>>,
}

impl PendingApprovalsState {
    pub(crate) fn new() -> Self {
        Self {
            requests: HashMap::new(),
            notifier: None,
        }
    }
}

/// Thread-safe storage for pending approvals (accessible from both GPUI and Tokio contexts)
pub type PendingApprovals = Arc<Mutex<PendingApprovalsState>>;

/// Shared approval flow used by shell tools, git tools, and any future tool that
/// requires user confirmation before executing.
///
/// `label` is a human-readable description prefixed with the tool domain
/// (e.g. `"[git] commit with message: …"`, `"[shell] rm -rf /tmp"`).
///
/// Returns `Ok(true)` if approved, `Ok(false)` if denied, or an error on
/// timeout / channel failure.
pub async fn request_execution_approval(
    pending: &PendingApprovals,
    approval_mode: &crate::settings::models::execution_settings::ApprovalMode,
    label: &str,
    is_sandboxed: bool,
) -> anyhow::Result<bool> {
    use crate::settings::models::execution_settings::ApprovalMode;
    use tracing::warn;

    match approval_mode {
        ApprovalMode::AutoApproveAll => return Ok(true),
        ApprovalMode::AutoApproveSandboxed if is_sandboxed => return Ok(true),
        _ => {}
    }

    let (tx, rx) = oneshot::channel();
    let request_id = uuid::Uuid::new_v4().to_string();

    {
        let mut state = pending.lock();
        state.requests.insert(request_id.clone(), tx);
        match &state.notifier {
            Some(notifier) => {
                if let Err(e) = notifier.send(ApprovalNotification {
                    id: request_id.clone(),
                    command: label.to_string(),
                    is_sandboxed,
                }) {
                    warn!(id = %request_id, error = ?e, "Failed to send approval notification");
                }
            }
            None => {
                warn!(id = %request_id, "Approval notifier not set - notification not sent!");
            }
        }
    }

    match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
        Ok(Ok(ApprovalDecision::Approved)) => Ok(true),
        Ok(Ok(ApprovalDecision::Denied)) => Ok(false),
        Ok(Err(_)) => Err(anyhow::anyhow!("Approval channel closed")),
        Err(_) => {
            pending.lock().requests.remove(&request_id);
            Err(anyhow::anyhow!("Approval timeout (5 minutes)"))
        }
    }
}

/// Per-agent store for pending execution approval requests.
/// Uses Arc<Mutex<>> internally to allow access from both GPUI and async Tokio contexts
#[derive(Clone)]
pub struct ExecutionApprovalStore {
    pending_requests: PendingApprovals,
    resolution_notifier: Option<mpsc::UnboundedSender<ApprovalResolution>>,
}

impl ExecutionApprovalStore {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(PendingApprovalsState::new())),
            resolution_notifier: None,
        }
    }

    /// Get a clone of the pending approvals handle for passing to async contexts
    pub fn get_pending_approvals(&self) -> PendingApprovals {
        self.pending_requests.clone()
    }

    /// Set the notification channels on an existing store for the current
    /// turn: the approval notifier lives on `PendingApprovals` itself, since
    /// that is the handle `request_execution_approval` actually holds.
    pub fn set_notifiers(
        &mut self,
        approval_tx: mpsc::UnboundedSender<ApprovalNotification>,
        resolution_tx: mpsc::UnboundedSender<ApprovalResolution>,
    ) {
        self.pending_requests.lock().notifier = Some(approval_tx);
        self.resolution_notifier = Some(resolution_tx);
    }

    /// Resolve an approval request by ID, returning whether it existed
    /// This is called from GPUI context when user clicks approve/deny button
    pub fn resolve(&self, id: &str, decision: ApprovalDecision) -> bool {
        let mut state = self.pending_requests.lock();
        if let Some(responder) = state.requests.remove(id) {
            let approved = matches!(decision, ApprovalDecision::Approved);
            let _ = responder.send(decision);

            // Notify stream that approval was resolved
            if let Some(tx) = &self.resolution_notifier {
                let _ = tx.send(ApprovalResolution {
                    id: id.to_string(),
                    approved,
                });
            }

            true
        } else {
            false
        }
    }
}

impl Default for ExecutionApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::models::execution_settings::ApprovalMode;

    /// AGE-246 / D7: two agents, each with its own `ExecutionApprovalStore`,
    /// must not cross-notify — a request made against agent A's store is
    /// delivered only to A's receiver, never B's.
    #[tokio::test]
    async fn per_agent_notifier_does_not_cross_notify_another_agents_store() {
        let mut store_a = ExecutionApprovalStore::new();
        let mut store_b = ExecutionApprovalStore::new();

        let (tx_a, mut rx_a) = mpsc::unbounded_channel();
        let (resolution_tx_a, _resolution_rx_a) = mpsc::unbounded_channel();
        store_a.set_notifiers(tx_a, resolution_tx_a);

        let (tx_b, mut rx_b) = mpsc::unbounded_channel();
        let (resolution_tx_b, _resolution_rx_b) = mpsc::unbounded_channel();
        store_b.set_notifiers(tx_b, resolution_tx_b);

        let pending_a = store_a.get_pending_approvals();
        let waiter = tokio::spawn({
            let pending_a = pending_a.clone();
            async move {
                request_execution_approval(
                    &pending_a,
                    &ApprovalMode::AlwaysAsk,
                    "rm -rf /tmp",
                    false,
                )
                .await
            }
        });

        let notification = rx_a
            .recv()
            .await
            .expect("agent A's receiver must see the notification");
        assert_eq!(notification.command, "rm -rf /tmp");
        assert!(
            rx_b.try_recv().is_err(),
            "agent B's receiver must not see agent A's request"
        );

        assert!(store_a.resolve(&notification.id, ApprovalDecision::Approved));
        assert!(waiter.await.unwrap().unwrap());
    }

    #[tokio::test]
    async fn auto_approve_all_skips_the_notifier_entirely() {
        let store = ExecutionApprovalStore::new();
        let pending = store.get_pending_approvals();

        let approved =
            request_execution_approval(&pending, &ApprovalMode::AutoApproveAll, "echo hi", false)
                .await
                .unwrap();

        assert!(approved);
    }

    #[test]
    fn resolve_reports_unknown_ids() {
        let store = ExecutionApprovalStore::new();
        assert!(!store.resolve("does-not-exist", ApprovalDecision::Approved));
    }
}
