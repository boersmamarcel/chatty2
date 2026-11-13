//! Resumable handles (RC-3, RC-4; `fabric-resumable-conversations` §4).
//!
//! A caller that names a handle on its `invoke_agent` keeps the callee's
//! conversation under that name. The first call under a name runs a fresh
//! worker, as any call does, and the broker saves what that worker said; a
//! later call under the same name spawns another fresh, one-shot worker that
//! restores the saved conversation before its first turn and works on the
//! same tree. Nothing outlives a task but the saved conversation: there is
//! never a long-lived worker process.
//!
//! A [`HandleStore`] lives in the root broker's memory only, so a
//! root-process restart loses every handle. A handle is addressed by its
//! root conversation and its name ([`HandleKey`]): one broker serves every
//! conversation on the desktop, and a conversation never sees or reaches
//! another's handles. Each handle has one queue: a call holds it for its
//! whole run, and `tokio`'s mutex is fair, so calls to one handle run one at
//! a time in arrival order.
//!
//! # Owner
//!
//! A handle's owner is whoever started its first task: the root
//! conversation, a node, or another handle (the worker of a handle's task
//! acts as that handle, so a handle it starts outlives the worker). The
//! owner and its ancestors may task it ([`HandleOwner::admits`]); the
//! broker gate decides that, as a row of `decide` (ADR-0023), not this
//! store. The owner records the kind of root its run hangs from and, for an
//! `External` root, the key that admitted it: an `External`-rooted run
//! reaches only its own key's handles, and no owner-rooted run reaches an
//! `External`-rooted one.
//!
//! # Lifecycle (RC-4)
//!
//! A handle ends when it has been idle for [`HANDLE_IDLE_LIMIT`], when its
//! owner drops it, or when its owner ends (the root conversation, the node,
//! or the owning handle). A conversation keeps at most
//! [`MAX_HANDLES_PER_CONVERSATION`] handles and
//! [`MAX_SAVED_BYTES_PER_CONVERSATION`] of saved conversations: past either,
//! the longest-idle handle ends. An ended handle's saved conversation is
//! deleted; its worktree and branch stay where they are (ADR-0017: removal
//! is a separate step after a merge, never part of teardown). Its name stays
//! ended for the rest of the conversation in this root process: tasking it
//! is [`HandleError::Expired`]. A conversation that ends forgets its names.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ConversationScope;

/// How long a handle may sit with no task before it ends.
pub const HANDLE_IDLE_LIMIT: Duration = Duration::from_secs(30 * 60);

/// The most handles one root conversation keeps; one more ends the
/// longest-idle.
pub const MAX_HANDLES_PER_CONVERSATION: usize = 8;

/// The most saved conversation one root conversation's handles keep, in
/// bytes of JSON (32 MiB); past it the longest-idle handles end.
pub const MAX_SAVED_BYTES_PER_CONVERSATION: usize = 32 * 1024 * 1024;

/// A worker's conversation as the broker keeps it: chatty-core's message
/// array, as JSON. chatty-fabric cannot name chatty-core's types (it would
/// be a dependency cycle), so chatty-core converts at its edge.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedConversation {
    pub data: Value,
    /// Its size as JSON: what counts against
    /// [`MAX_SAVED_BYTES_PER_CONVERSATION`].
    pub bytes: usize,
}

impl SavedConversation {
    pub fn new(data: Value) -> Self {
        let bytes = serde_json::to_vec(&data).map_or(0, |json| json.len());
        Self { data, bytes }
    }

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
        Self::new(Value::Array(data))
    }
}

/// The worktree a handle's workers work in: where it is and the branch it
/// is on. chatty-core attaches a resumed worker to it (`worker_tree::attach`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerTreeRef {
    pub path: PathBuf,
    pub branch: String,
}

/// A handle's address: its root conversation and its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandleKey {
    pub scope: ConversationScope,
    pub name: String,
}

impl HandleKey {
    pub fn new(scope: ConversationScope, name: impl Into<String>) -> Self {
        Self {
            scope,
            name: name.into(),
        }
    }
}

/// Who a handle's owner, or a caller of one, is, within its conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HandleParty {
    /// The root conversation.
    Root,
    /// A handle, by name: the worker of any of its tasks.
    Handle(String),
    /// A node that serves no handle, by its broker name.
    Node(String),
}

impl fmt::Display for HandleParty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => f.write_str(crate::ROOT_NAME),
            Self::Handle(name) => write!(f, "handle '{name}'"),
            Self::Node(name) => write!(f, "node '{name}'"),
        }
    }
}

/// The root a run hangs from (ADR-0023 § 2): the human's own, or a typed
/// root a `Remote` or `External` caller started.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RunRoot {
    /// The local root or a hosted root conversation.
    Owner,
    /// A run an ADR-0022 stream peer started.
    Remote,
    /// A run an `External` caller started, by the key that admitted it.
    External { key: String },
}

/// Whose a handle is: the root kind and key of the run that started it, and
/// its owner followed by the owner's ancestors, nearest first, ending at
/// [`HandleParty::Root`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleOwner {
    pub root: RunRoot,
    pub line: Vec<HandleParty>,
}

impl HandleOwner {
    /// `owner` and `ancestors` (nearest first); the root conversation ends
    /// the line whether or not `ancestors` names it.
    pub fn new(root: RunRoot, owner: HandleParty, ancestors: Vec<HandleParty>) -> Self {
        let mut line = vec![owner];
        line.extend(ancestors);
        if line.last() != Some(&HandleParty::Root) {
            line.push(HandleParty::Root);
        }
        Self { root, line }
    }

    /// Who started the handle's first task.
    pub fn owner(&self) -> &HandleParty {
        &self.line[0]
    }

    /// Whether `party`, in a run rooted at `root`, may task the handle: it
    /// is the owner or one of the owner's ancestors, under the same root
    /// kind and key.
    pub fn admits(&self, root: &RunRoot, party: &HandleParty) -> bool {
        self.root == *root && self.line.contains(party)
    }
}

/// Why a call naming a handle was refused, typed for the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandleError {
    /// The handle ended: idle too long, dropped, evicted, or its owner
    /// ended. Its conversation is gone.
    #[serde(rename = "handle_expired")]
    #[error(
        "handle_expired: handle '{name}' has ended (idle for 30 min, dropped, evicted, or its \
         owner ended) and its conversation is gone; call without a handle, or under a new name, \
         to start fresh"
    )]
    Expired { name: String },
    /// The caller is neither the handle's owner nor one of its ancestors.
    #[error("not_owner: {caller} may not task handle '{name}': it belongs to {owner}")]
    NotOwner {
        name: String,
        caller: String,
        owner: String,
    },
}

/// What a conversation's name stands for, as the gate reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleStatus {
    /// No handle goes by it: a call under it starts one.
    Free,
    /// A live handle, whoever is running it.
    Live(HandleOwner),
    /// A handle that ended.
    Ended,
}

/// What a handle's next task resumes from.
#[derive(Debug, Clone, PartialEq)]
pub struct Handle {
    pub name: String,
    /// The agent (spec) its workers run as. A resume must name the same one.
    pub agent: String,
    /// Everything its workers said, in order.
    pub conversation: SavedConversation,
    /// The tree its workers work in; `None` when they share their caller's.
    pub tree: Option<WorkerTreeRef>,
    /// The commit the last task started from, recorded at spawn: its
    /// evidence is `last_task_base..HEAD`.
    pub last_task_base: Option<String>,
}

/// One live handle as a roster row reads it (RO-4): what the store knows
/// without the pending mail (the broker keeps that apart) and without git.
#[derive(Debug, Clone, PartialEq)]
pub struct HandleListing {
    pub name: String,
    pub owner: HandleOwner,
    /// A call is running or queued on it.
    pub busy: bool,
    /// The agent (spec) its workers run as, once a task has saved one.
    pub agent: Option<String>,
    /// Its worktree and branch, once a task has saved one.
    pub tree: Option<WorkerTreeRef>,
    /// How long since its last call let go of it.
    pub idle_for: Duration,
}

/// The clock a store measures idleness by.
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

/// One live handle.
struct Entry {
    /// Tells this entry from a later one under the same name.
    id: u64,
    owner: HandleOwner,
    /// Held for the whole run of each task: the handle's queue.
    queue: Arc<tokio::sync::Mutex<()>>,
    /// Empty until a task under the name has captured a conversation.
    saved: Option<Handle>,
    /// Calls running or queued on it.
    busy: usize,
    /// When its last call let go of it.
    idle_since: SystemTime,
}

#[derive(Default)]
struct Inner {
    live: HashMap<HandleKey, Entry>,
    ended: HashSet<HandleKey>,
    /// The handle each handle worker serves, by node name.
    workers: HashMap<String, HandleKey>,
    next_id: u64,
}

/// The root broker's handles.
pub struct HandleStore {
    inner: Mutex<Inner>,
    clock: Clock,
}

impl Default for HandleStore {
    fn default() -> Self {
        Self::with_clock(Arc::new(SystemTime::now))
    }
}

impl fmt::Debug for HandleStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.lock();
        f.debug_struct("HandleStore")
            .field("live", &inner.live.len())
            .field("ended", &inner.ended.len())
            .finish()
    }
}

/// [`HandleStore::begin`] found the name changed since the gate read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleChanged;

impl HandleStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that tells the time by `clock` (tests use a fake one).
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            inner: Mutex::default(),
            clock,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What `key` stands for now. Idle handles end first.
    pub fn status(&self, key: &HandleKey) -> HandleStatus {
        let mut inner = self.lock();
        inner.sweep((self.clock)());
        inner.status(key)
    }

    /// Whether `key` is a live handle that is idle and holds a saved
    /// conversation: not mid-task, not queued on, not yet made (TM-4).
    pub fn is_idle(&self, key: &HandleKey) -> bool {
        let mut inner = self.lock();
        inner.sweep((self.clock)());
        inner
            .live
            .get(key)
            .is_some_and(|entry| entry.busy == 0 && entry.saved.is_some())
    }

    /// The handle whose task the node `node` is running, if it runs one.
    pub fn served_by(&self, node: &str) -> Option<HandleKey> {
        self.lock().workers.get(node).cloned()
    }

    /// The names of the live handles of `scope`, sorted.
    pub fn names(&self, scope: &ConversationScope) -> Vec<String> {
        let mut inner = self.lock();
        inner.sweep((self.clock)());
        let mut names: Vec<String> = inner
            .live
            .keys()
            .filter(|key| key.scope == *scope)
            .map(|key| key.name.clone())
            .collect();
        names.sort();
        names
    }

    /// The live handles of `scope`, by name (RO-4). Idle handles end first.
    pub fn listing(&self, scope: &ConversationScope) -> Vec<HandleListing> {
        let now = (self.clock)();
        let mut inner = self.lock();
        inner.sweep(now);
        let mut rows: Vec<HandleListing> = inner
            .live
            .iter()
            .filter(|(key, _)| key.scope == *scope)
            .map(|(key, entry)| HandleListing {
                name: key.name.clone(),
                owner: entry.owner.clone(),
                busy: entry.busy > 0,
                agent: entry.saved.as_ref().map(|saved| saved.agent.clone()),
                tree: entry.saved.as_ref().and_then(|saved| saved.tree.clone()),
                idle_for: if entry.busy > 0 {
                    Duration::ZERO
                } else {
                    now.duration_since(entry.idle_since).unwrap_or_default()
                },
            })
            .collect();
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        rows
    }

    /// Join `key`'s queue as a call the gate granted on `expected`: a
    /// [`HandleStatus::Free`] name becomes a handle owned by `owner` (the
    /// conversation's longest-idle handle ending if it is full), and a live
    /// one gains a waiting call. [`HandleChanged`] when the name no longer
    /// stands for what the gate read.
    pub fn begin(
        self: &Arc<Self>,
        key: HandleKey,
        owner: HandleOwner,
        expected: &HandleStatus,
    ) -> Result<HandleTicket, HandleChanged> {
        let now = (self.clock)();
        let mut inner = self.lock();
        inner.sweep(now);
        if inner.status(&key) != *expected {
            return Err(HandleChanged);
        }
        let id = match expected {
            HandleStatus::Ended => return Err(HandleChanged),
            HandleStatus::Live(_) => {
                let entry = inner.live.get_mut(&key).ok_or(HandleChanged)?;
                entry.busy += 1;
                entry.id
            }
            HandleStatus::Free => {
                let count = inner.live.keys().filter(|k| k.scope == key.scope).count();
                if count >= MAX_HANDLES_PER_CONVERSATION
                    && let Some(oldest) = inner.longest_idle(&key.scope, &key)
                {
                    tracing::info!(handle = %oldest.name, "A conversation's ninth handle ends its longest-idle one");
                    inner.end(&oldest);
                }
                let id = inner.next_id;
                inner.next_id += 1;
                inner.live.insert(
                    key.clone(),
                    Entry {
                        id,
                        owner,
                        queue: Arc::default(),
                        saved: None,
                        busy: 1,
                        idle_since: now,
                    },
                );
                id
            }
        };
        let queue = inner
            .live
            .get(&key)
            .map(|entry| entry.queue.clone())
            .ok_or(HandleChanged)?;
        Ok(HandleTicket {
            store: self.clone(),
            key,
            id,
            queue,
            worker: Mutex::new(None),
        })
    }

    /// End `key` (its owner dropped it): its conversation is deleted, and
    /// every handle it owns ends with it. `false` when it was not live.
    pub fn drop_handle(&self, key: &HandleKey) -> bool {
        let mut inner = self.lock();
        let live = inner.live.contains_key(key);
        inner.end(key);
        live
    }

    /// `party` of `scope` ended: every handle it owns ends.
    pub fn owner_ended(&self, scope: &ConversationScope, party: &HandleParty) {
        self.lock().owner_ended(scope, party);
    }

    /// The root conversation `scope` ended: every handle of it ends, and
    /// the conversation's ended names are forgotten with it — nobody is
    /// left to call them.
    pub fn end_conversation(&self, scope: &ConversationScope) {
        let mut inner = self.lock();
        let live: Vec<HandleKey> = inner
            .live
            .keys()
            .filter(|key| key.scope == *scope)
            .cloned()
            .collect();
        for key in live {
            inner.end(&key);
        }
        inner.ended.retain(|key| key.scope != *scope);
        inner.workers.retain(|_, key| key.scope != *scope);
    }
}

impl Inner {
    fn status(&self, key: &HandleKey) -> HandleStatus {
        match self.live.get(key) {
            Some(entry) => HandleStatus::Live(entry.owner.clone()),
            None if self.ended.contains(key) => HandleStatus::Ended,
            None => HandleStatus::Free,
        }
    }

    /// End every handle idle for [`HANDLE_IDLE_LIMIT`] by `now`.
    fn sweep(&mut self, now: SystemTime) {
        let idle: Vec<HandleKey> = self
            .live
            .iter()
            .filter(|(_, entry)| {
                entry.busy == 0
                    && now
                        .duration_since(entry.idle_since)
                        .is_ok_and(|idle| idle >= HANDLE_IDLE_LIMIT)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in idle {
            tracing::info!(handle = %key.name, "A handle ended after 30 min idle");
            self.end(&key);
        }
    }

    /// The handle of `scope` other than `except` that has been idle
    /// longest; one with a call running or queued never is.
    fn longest_idle(&self, scope: &ConversationScope, except: &HandleKey) -> Option<HandleKey> {
        self.live
            .iter()
            .filter(|(key, entry)| key.scope == *scope && *key != except && entry.busy == 0)
            .min_by_key(|(_, entry)| entry.idle_since)
            .map(|(key, _)| key.clone())
    }

    /// End `key` and, after it, every handle it owns.
    fn end(&mut self, key: &HandleKey) {
        if self.live.remove(key).is_some() {
            self.ended.insert(key.clone());
            self.owner_ended(&key.scope, &HandleParty::Handle(key.name.clone()));
        }
    }

    fn owner_ended(&mut self, scope: &ConversationScope, party: &HandleParty) {
        let owned: Vec<HandleKey> = self
            .live
            .iter()
            .filter(|(key, entry)| key.scope == *scope && entry.owner.owner() == party)
            .map(|(key, _)| key.clone())
            .collect();
        for key in owned {
            self.end(&key);
        }
    }

    /// End `scope`'s longest-idle handles, never `keep`, until its saved
    /// conversations fit [`MAX_SAVED_BYTES_PER_CONVERSATION`].
    fn fit_bytes(&mut self, keep: &HandleKey) {
        loop {
            let total: usize = self
                .live
                .iter()
                .filter(|(key, _)| key.scope == keep.scope)
                .filter_map(|(_, entry)| entry.saved.as_ref())
                .map(|saved| saved.conversation.bytes)
                .sum();
            if total <= MAX_SAVED_BYTES_PER_CONVERSATION {
                return;
            }
            let Some(oldest) = self.longest_idle(&keep.scope, keep) else {
                return;
            };
            tracing::info!(handle = %oldest.name, total, "Saved conversations over the cap end the longest-idle handle");
            self.end(&oldest);
        }
    }
}

/// One call's place on a handle, from the gate's grant until the call ends.
/// Dropping it lets go of the handle.
pub struct HandleTicket {
    store: Arc<HandleStore>,
    key: HandleKey,
    id: u64,
    queue: Arc<tokio::sync::Mutex<()>>,
    /// The node running this call's task, once it runs.
    worker: Mutex<Option<String>>,
}

impl HandleTicket {
    pub fn key(&self) -> &HandleKey {
        &self.key
    }

    /// The handle's queue: lock it ([`tokio::sync::Mutex::lock_owned`]) to
    /// wait for the calls ahead of this one.
    pub fn queue(&self) -> Arc<tokio::sync::Mutex<()>> {
        self.queue.clone()
    }

    /// What this call resumes from, once it holds the queue: the saved
    /// handle, or `None` for its first task. [`HandleError::Expired`] when
    /// the handle ended while the call waited.
    pub fn saved(&self) -> Result<Option<Handle>, HandleError> {
        let inner = self.store.lock();
        match inner.live.get(&self.key) {
            Some(entry) if entry.id == self.id => Ok(entry.saved.clone()),
            _ => Err(HandleError::Expired {
                name: self.key.name.clone(),
            }),
        }
    }

    /// The node `node` runs this call's task: the calls it makes are the
    /// handle's.
    pub fn served_by(&self, node: &str) {
        self.store
            .lock()
            .workers
            .insert(node.to_string(), self.key.clone());
        *self.worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(node.to_string());
    }

    /// Keep `handle` as what the next task resumes from, unless the handle
    /// ended meanwhile; then the conversation's longest-idle handles end
    /// until its saved conversations fit the cap. `false` when it had ended.
    pub fn save(&self, handle: Handle) -> bool {
        let mut inner = self.store.lock();
        match inner.live.get_mut(&self.key) {
            Some(entry) if entry.id == self.id => entry.saved = Some(handle),
            _ => return false,
        }
        inner.fit_bytes(&self.key);
        true
    }

    /// The task captured nothing to resume from: the handle keeps nothing,
    /// and goes once no call holds it.
    pub fn clear(&self) {
        if let Some(entry) = self.store.lock().live.get_mut(&self.key)
            && entry.id == self.id
        {
            entry.saved = None;
        }
    }
}

impl Drop for HandleTicket {
    fn drop(&mut self) {
        let now = (self.store.clock)();
        let mut inner = self.store.lock();
        // The worker is reaped before its ticket goes: it calls as the
        // handle no more.
        if let Some(node) = self
            .worker
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            inner.workers.remove(&node);
        }
        let Some(entry) = inner.live.get_mut(&self.key) else {
            return;
        };
        if entry.id != self.id {
            return;
        }
        entry.busy = entry.busy.saturating_sub(1);
        entry.idle_since = now;
        if entry.busy == 0 && entry.saved.is_none() {
            // Nothing to resume: the name is free again.
            inner.live.remove(&self.key);
        }
    }
}

#[cfg(test)]
#[path = "handle_tests.rs"]
mod tests;
