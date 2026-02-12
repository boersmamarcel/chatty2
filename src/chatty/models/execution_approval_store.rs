use gpui::Global;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::sync::oneshot;

/// Decision for an execution approval request
#[derive(Clone, Debug)]
pub enum ApprovalDecision {
    Approved,
    Denied,
}

/// Request for user approval to execute a command
#[allow(dead_code)]
pub struct ExecutionApprovalRequest {
    /// Unique ID for tracking this request
    pub id: String,
    /// Command to be executed
    pub command: String,
    /// Whether execution will be sandboxed
    pub is_sandboxed: bool,
    /// When the request was created (for timeout tracking)
    pub created_at: SystemTime,
    /// Channel to send approval decision back to waiting execution
    pub responder: oneshot::Sender<ApprovalDecision>,
}

/// Thread-safe storage for pending approvals (accessible from both GPUI and Tokio contexts)
pub type PendingApprovals = Arc<Mutex<HashMap<String, ExecutionApprovalRequest>>>;

/// Global store for pending execution approval requests
/// Uses Arc<Mutex<>> internally to allow access from both GPUI and async Tokio contexts
pub struct ExecutionApprovalStore {
    pending_requests: PendingApprovals,
}

impl Global for ExecutionApprovalStore {}

impl ExecutionApprovalStore {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get a clone of the pending approvals handle for passing to async contexts
    pub fn get_pending_approvals(&self) -> PendingApprovals {
        self.pending_requests.clone()
    }

    /// Resolve an approval request by ID, returning whether it existed
    /// This is called from GPUI context when user clicks approve/deny button
    pub fn resolve(&self, id: &str, decision: ApprovalDecision) -> bool {
        let mut pending = self.pending_requests.lock().unwrap();
        if let Some(request) = pending.remove(id) {
            // Send decision through channel (ignore error if receiver dropped)
            let _ = request.responder.send(decision);
            true
        } else {
            false
        }
    }

    /// Get a pending request by ID for display purposes
    #[allow(dead_code)]
    pub fn get_pending(&self, id: &str) -> Option<(String, String, bool)> {
        let pending = self.pending_requests.lock().unwrap();
        pending
            .get(id)
            .map(|req| (req.id.clone(), req.command.clone(), req.is_sandboxed))
    }

    /// Clear all pending requests (e.g., on shutdown)
    #[allow(dead_code)]
    pub fn clear_all(&self) {
        let mut pending = self.pending_requests.lock().unwrap();
        for (_id, request) in pending.drain() {
            // Auto-deny all pending approvals
            let _ = request.responder.send(ApprovalDecision::Denied);
        }
    }
}

impl Default for ExecutionApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}
