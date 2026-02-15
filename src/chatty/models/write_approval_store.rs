use gpui::Global;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// Decision for a filesystem write approval request
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum WriteApprovalDecision {
    Approved,
    Denied,
}

/// Types of write operations that require approval
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum WriteOperation {
    /// Writing/overwriting a file
    WriteFile {
        path: String,
        is_overwrite: bool,
        content_preview: String,
    },
    /// Deleting a file
    DeleteFile { path: String },
    /// Moving/renaming a file
    MoveFile { source: String, destination: String },
    /// Applying a diff to a file
    ApplyDiff {
        path: String,
        old_preview: String,
        new_preview: String,
    },
}

#[allow(dead_code)]
impl WriteOperation {
    /// Get a human-readable description for display
    pub fn description(&self) -> String {
        match self {
            WriteOperation::WriteFile {
                path, is_overwrite, ..
            } => {
                if *is_overwrite {
                    format!("Overwrite file: {}", path)
                } else {
                    format!("Create file: {}", path)
                }
            }
            WriteOperation::DeleteFile { path } => format!("Delete file: {}", path),
            WriteOperation::MoveFile {
                source,
                destination,
            } => format!("Move: {} → {}", source, destination),
            WriteOperation::ApplyDiff { path, .. } => format!("Edit file: {}", path),
        }
    }

    /// Whether this is a destructive operation (delete, overwrite)
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            WriteOperation::DeleteFile { .. }
                | WriteOperation::WriteFile {
                    is_overwrite: true,
                    ..
                }
        )
    }
}

/// Request for user approval to perform a filesystem write operation
#[allow(dead_code)]
pub struct WriteApprovalRequest {
    /// Unique ID for tracking this request
    pub id: String,
    /// The operation to be approved
    pub operation: WriteOperation,
    /// Channel to send approval decision back to waiting tool
    pub responder: oneshot::Sender<WriteApprovalDecision>,
}

/// Thread-safe storage for pending write approvals
pub type PendingWriteApprovals = Arc<Mutex<HashMap<String, WriteApprovalRequest>>>;

/// Global store for pending filesystem write approval requests
pub struct WriteApprovalStore {
    pending_requests: PendingWriteApprovals,
}

impl Global for WriteApprovalStore {}

impl WriteApprovalStore {
    pub fn new() -> Self {
        Self {
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get a clone of the pending approvals handle for passing to async contexts
    pub fn get_pending_approvals(&self) -> PendingWriteApprovals {
        self.pending_requests.clone()
    }

    /// Resolve an approval request by ID
    #[allow(dead_code)]
    pub fn resolve(&self, id: &str, decision: WriteApprovalDecision) -> bool {
        let mut pending = self.pending_requests.lock().unwrap();
        if let Some(request) = pending.remove(id) {
            let _ = request.responder.send(decision);
            true
        } else {
            false
        }
    }
}

impl Default for WriteApprovalStore {
    fn default() -> Self {
        Self::new()
    }
}
