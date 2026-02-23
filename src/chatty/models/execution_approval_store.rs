use gpui::Global;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use tokio::sync::{mpsc, oneshot};

// Global notification senders (set once per message send, cleared between messages)
static GLOBAL_APPROVAL_NOTIFIER: OnceLock<
    Mutex<Option<mpsc::UnboundedSender<ApprovalNotification>>>,
> = OnceLock::new();

/// Set the global approval notifier for the current message
pub fn set_global_approval_notifier(tx: mpsc::UnboundedSender<ApprovalNotification>) {
    GLOBAL_APPROVAL_NOTIFIER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .replace(tx);
}

/// Notify via global channel (called by shell tools)
pub fn notify_approval_via_global(id: String, command: String, is_sandboxed: bool) {
    use tracing::{debug, warn};

    if let Some(guard) = GLOBAL_APPROVAL_NOTIFIER.get() {
        if let Some(tx) = guard.lock().unwrap().as_ref() {
            match tx.send(ApprovalNotification {
                id: id.clone(),
                command,
                is_sandboxed,
            }) {
                Ok(_) => {
                    debug!(id = %id, "Successfully sent approval notification via global channel");
                }
                Err(e) => {
                    warn!(id = %id, error = ?e, "Failed to send approval notification via global channel");
                }
            }
        } else {
            warn!(id = %id, "Global approval notifier not set - notification not sent!");
        }
    } else {
        warn!(id = %id, "Global approval notifier not initialized - notification not sent!");
    }
}

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

/// Request for user approval to execute a command
pub struct ExecutionApprovalRequest {
    /// Unique ID for tracking this request
    #[allow(dead_code)]
    pub id: String,
    /// Command to be executed
    #[allow(dead_code)]
    pub command: String,
    /// Whether execution will be sandboxed
    #[allow(dead_code)]
    pub is_sandboxed: bool,
    /// When the request was created (for timeout tracking)
    #[allow(dead_code)]
    pub created_at: SystemTime,
    /// Channel to send approval decision back to waiting execution
    pub responder: oneshot::Sender<ApprovalDecision>,
}

/// Thread-safe storage for pending approvals (accessible from both GPUI and Tokio contexts)
pub type PendingApprovals = Arc<Mutex<HashMap<String, ExecutionApprovalRequest>>>;

/// Global store for pending execution approval requests
/// Uses Arc<Mutex<>> internally to allow access from both GPUI and async Tokio contexts
#[derive(Clone)]
pub struct ExecutionApprovalStore {
    pending_requests: PendingApprovals,
    approval_notifier: Option<mpsc::UnboundedSender<ApprovalNotification>>,
    resolution_notifier: Option<mpsc::UnboundedSender<ApprovalResolution>>,
}

impl Global for ExecutionApprovalStore {}

impl ExecutionApprovalStore {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            approval_notifier: None,
            resolution_notifier: None,
        }
    }

    /// Get a clone of the pending approvals handle for passing to async contexts
    pub fn get_pending_approvals(&self) -> PendingApprovals {
        self.pending_requests.clone()
    }

    /// Set the notification channels on an existing store
    /// This allows updating the notifiers without replacing the entire store
    pub fn set_notifiers(
        &mut self,
        approval_tx: mpsc::UnboundedSender<ApprovalNotification>,
        resolution_tx: mpsc::UnboundedSender<ApprovalResolution>,
    ) {
        self.approval_notifier = Some(approval_tx);
        self.resolution_notifier = Some(resolution_tx);
    }

    /// Resolve an approval request by ID, returning whether it existed
    /// This is called from GPUI context when user clicks approve/deny button
    pub fn resolve(&self, id: &str, decision: ApprovalDecision) -> bool {
        let mut pending = self.pending_requests.lock().unwrap();
        if let Some(request) = pending.remove(id) {
            let approved = matches!(decision, ApprovalDecision::Approved);
            let _ = request.responder.send(decision);

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
