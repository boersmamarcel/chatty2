//! Resumable handles (RC-3, `fabric-resumable-conversations` §4).
//!
//! A caller that names a handle on its `invoke_agent` keeps the callee's
//! conversation under that name. The first call under a name runs a fresh
//! worker, as any call does, and the broker saves what that worker said; a
//! later call under the same name spawns another fresh, one-shot worker that
//! restores the saved conversation before its first turn and works on the
//! same tree. Nothing outlives a task but the saved conversation: there is
//! never a long-lived worker process.
//!
//! A [`HandleStore`] lives in the root broker's memory only. Each handle is
//! keyed by its owner — the caller that named it — and its name, so two
//! callers that pick the same name never share a conversation. The store's
//! per-handle slot is also the handle's queue: a call holds its slot for its
//! whole run, and `tokio`'s mutex is fair, so calls to one handle run one at
//! a time in arrival order.
//!
//! What a handle is not yet: it never expires, has no cap and is never
//! listed. The lifecycle — the owner rule, the scope check, the idle
//! timeout, caps and `handle_expired` — is RC-4's; [`Handle::idle_since`],
//! [`Handle::owner`] and [`Handle::scope`] are recorded for it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde_json::Value;

use crate::ConversationScope;

/// A worker's conversation as the broker keeps it: chatty-core's message
/// array, as JSON. chatty-fabric cannot name chatty-core's types (it would
/// be a dependency cycle), so chatty-core converts at its edge.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedConversation {
    pub data: Value,
}

impl SavedConversation {
    /// This conversation followed by `turns`, a later task's captured
    /// messages: what the next resume restores. A value that is not an
    /// array contributes nothing.
    pub fn extended(&self, turns: &Value) -> Self {
        let mut data = match &self.data {
            Value::Array(items) => items.clone(),
            _ => Vec::new(),
        };
        if let Value::Array(items) = turns {
            data.extend(items.iter().cloned());
        }
        Self {
            data: Value::Array(data),
        }
    }
}

/// The worktree a handle's workers work in: where it is and the branch it
/// is on. chatty-core attaches a resumed worker to it (`worker_tree::attach`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTreeRef {
    pub path: PathBuf,
    pub branch: String,
}

/// One resumable handle.
#[derive(Debug, Clone, PartialEq)]
pub struct Handle {
    /// The name the owner gave it on `invoke_agent`.
    pub name: String,
    /// The agent (spec) its workers run as. A resume must name the same one.
    pub agent: String,
    /// The caller that named it: a node's broker name, or
    /// [`ROOT_NAME`](crate::ROOT_NAME).
    pub owner: String,
    /// The owner's conversation scope.
    pub scope: ConversationScope,
    /// Everything its workers said, in order.
    pub conversation: SavedConversation,
    /// The tree its workers work in; `None` when they share their caller's.
    pub tree: Option<WorkerTreeRef>,
    /// The commit the last task started from, recorded at spawn: its
    /// evidence is `last_task_base..HEAD`.
    pub last_task_base: Option<String>,
    /// When its last task ended.
    pub idle_since: SystemTime,
}

/// One handle's slot: empty until a task under its name has captured a
/// conversation. Locked for the whole run of each task, which is the queue.
pub type HandleSlot = Arc<tokio::sync::Mutex<Option<Handle>>>;

/// The root broker's handles, by owner and name.
#[derive(Debug, Default)]
pub struct HandleStore {
    slots: Mutex<HashMap<(String, String), HandleSlot>>,
}

impl HandleStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The slot of `owner`'s handle `name`, made empty the first time it is
    /// asked for. Lock it ([`tokio::sync::Mutex::lock_owned`]) to queue
    /// behind the calls already holding it.
    pub fn slot(&self, owner: &str, name: &str) -> HandleSlot {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry((owner.to_string(), name.to_string()))
            .or_default()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn a_resumed_conversation_is_the_old_one_then_the_new_turns() {
        let saved = SavedConversation {
            data: json!([{"role": "user", "content": "one"}]),
        };
        let next = saved.extended(&json!([{"role": "assistant", "content": "two"}]));
        assert_eq!(
            next.data,
            json!([
                {"role": "user", "content": "one"},
                {"role": "assistant", "content": "two"}
            ])
        );
    }

    #[test]
    fn one_name_under_two_owners_is_two_handles() {
        let store = HandleStore::new();
        assert!(Arc::ptr_eq(
            &store.slot("root", "fix"),
            &store.slot("root", "fix")
        ));
        assert!(!Arc::ptr_eq(
            &store.slot("root", "fix"),
            &store.slot("lead-0", "fix")
        ));
    }

    /// Calls to one handle run one at a time, in the order they arrived.
    #[tokio::test(start_paused = true)]
    async fn a_handle_s_calls_queue_in_arrival_order() {
        let store = Arc::new(HandleStore::new());
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = store.slot("root", "h").lock_owned().await;
        let mut waiting = Vec::new();
        for n in 0..3 {
            let slot = store.slot("root", "h");
            let order = order.clone();
            waiting.push(tokio::spawn(async move {
                let _held = slot.lock_owned().await;
                order.lock().unwrap().push(n);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }));
            // Each one queued before the next arrives.
            tokio::task::yield_now().await;
        }
        assert!(order.lock().unwrap().is_empty(), "the first call holds it");
        drop(first);
        for task in waiting {
            task.await.unwrap();
        }
        assert_eq!(*order.lock().unwrap(), vec![0, 1, 2]);
    }
}
