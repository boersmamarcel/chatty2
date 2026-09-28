//! Which runs are in flight between which nodes.
//!
//! A run is one task a caller handed a callee. When a callee delegates in
//! turn, its run gains an outstanding child; the per-run endpoint permits of
//! BI-6 release a run's permit while that count is above zero.
//!
//! Each run carries the [`CallChain`] the broker stamped on it (DP-2): a
//! call its callee makes runs under that chain plus the next callee, which
//! is how the broker refuses a cycle or a too-deep call without asking the
//! caller where it is.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::delegation::CallChain;
use crate::directory::NodeId;

/// A run's broker-local id. Never reused within a root process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(u64);

impl RunId {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "run-{}", self.0)
    }
}

/// One run in flight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEntry {
    /// The calling node; `None` for the in-process root, which is not one.
    pub caller: Option<NodeId>,
    pub callee: NodeId,
    /// The caller's run this one was started from; `None` for a root task.
    pub parent: Option<RunId>,
    /// The chain the broker stamped on this run (DP-2).
    pub chain: CallChain,
    /// Child runs started from this one that have not closed yet.
    pub outstanding_children: u32,
    /// The call that opened this run has ended ([`TaskTable::release`]):
    /// it closes as soon as its last child does.
    #[serde(default)]
    pub released: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskTableError {
    #[error("unknown run {0}")]
    UnknownRun(RunId),
    #[error("{0} still has {1} outstanding child run(s)")]
    ChildrenOutstanding(RunId, u32),
}

/// Every run in flight in one root process.
#[derive(Debug, Default)]
pub struct TaskTable {
    runs: BTreeMap<RunId, TaskEntry>,
    next_id: u64,
}

impl TaskTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a run from `caller` to `callee`. A `parent` run gains an
    /// outstanding child until this run closes.
    pub fn open(
        &mut self,
        caller: Option<NodeId>,
        callee: NodeId,
        parent: Option<RunId>,
        chain: CallChain,
    ) -> Result<RunId, TaskTableError> {
        if let Some(parent) = parent {
            self.runs
                .get_mut(&parent)
                .ok_or(TaskTableError::UnknownRun(parent))?
                .outstanding_children += 1;
        }
        let id = RunId(self.next_id);
        self.next_id += 1;
        self.runs.insert(
            id,
            TaskEntry {
                caller,
                callee,
                parent,
                chain,
                outstanding_children: 0,
                released: false,
            },
        );
        Ok(id)
    }

    pub fn get(&self, run: RunId) -> Option<&TaskEntry> {
        self.runs.get(&run)
    }

    /// Close `run` and return its entry. Its parent's outstanding count
    /// drops by one. A run with children still open cannot close: its
    /// callee is still waiting on them.
    pub fn close(&mut self, run: RunId) -> Result<TaskEntry, TaskTableError> {
        let entry = self.runs.get(&run).ok_or(TaskTableError::UnknownRun(run))?;
        if entry.outstanding_children > 0 {
            return Err(TaskTableError::ChildrenOutstanding(
                run,
                entry.outstanding_children,
            ));
        }
        let entry = self.runs.remove(&run).expect("checked above");
        if let Some(parent) = entry.parent {
            self.child_closed(parent);
        }
        Ok(entry)
    }

    /// The call that opened `run` has ended, however it ended. The run
    /// closes now, or — while children it started are still open, as when
    /// a cancelled caller is reaped before its callees are — as soon as the
    /// last of them closes. An unknown run is already gone.
    pub fn release(&mut self, run: RunId) {
        let Some(entry) = self.runs.get_mut(&run) else {
            return;
        };
        entry.released = true;
        if entry.outstanding_children == 0 {
            let _ = self.close(run);
        }
    }

    fn child_closed(&mut self, parent: RunId) {
        let Some(entry) = self.runs.get_mut(&parent) else {
            return;
        };
        entry.outstanding_children -= 1;
        if entry.released && entry.outstanding_children == 0 {
            let _ = self.close(parent);
        }
    }

    /// Runs `node` is serving as callee.
    pub fn served_by(&self, node: NodeId) -> impl Iterator<Item = (RunId, &TaskEntry)> + '_ {
        self.runs
            .iter()
            .filter(move |(_, entry)| entry.callee == node)
            .map(|(id, entry)| (*id, entry))
    }

    pub fn len(&self) -> usize {
        self.runs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::{ConversationScope, Directory};

    fn chain() -> CallChain {
        CallChain::root("t")
    }

    #[test]
    fn a_child_run_is_outstanding_on_its_parent_until_it_closes() {
        let mut dir = Directory::new();
        let s = ConversationScope::new("c");
        let root = dir.admit("leader", None, s.clone()).unwrap().id();
        let sub = dir.admit("sub", Some(root), s.clone()).unwrap().id();
        let leaf = dir.admit("leaf", Some(sub), s).unwrap().id();

        let mut table = TaskTable::new();
        let top = table.open(Some(root), sub, None, chain()).unwrap();
        let a = table.open(Some(sub), leaf, Some(top), chain()).unwrap();
        let b = table.open(Some(sub), leaf, Some(top), chain()).unwrap();
        assert_eq!(table.get(top).unwrap().outstanding_children, 2);
        assert_eq!(table.served_by(leaf).count(), 2);

        assert_eq!(
            table.close(top),
            Err(TaskTableError::ChildrenOutstanding(top, 2))
        );
        table.close(b).unwrap();
        assert_eq!(table.get(top).unwrap().outstanding_children, 1);
        table.close(a).unwrap();
        assert_eq!(table.get(top).unwrap().outstanding_children, 0);
        let entry = table.close(top).unwrap();
        assert_eq!((entry.caller, entry.callee), (Some(root), sub));
        assert!(table.is_empty());

        // Ids are not reused after a close.
        let next = table.open(Some(root), sub, None, chain()).unwrap();
        assert!(next > b);
        assert_eq!(
            table.open(Some(root), sub, Some(top), chain()),
            Err(TaskTableError::UnknownRun(top))
        );
    }

    /// A released run whose children are still open closes with the last
    /// of them, and takes a released grandparent with it.
    #[test]
    fn a_released_run_closes_with_its_last_child() {
        let mut dir = Directory::new();
        let s = ConversationScope::new("c");
        let a = dir.admit("a", None, s.clone()).unwrap().id();
        let b = dir.admit("b", Some(a), s.clone()).unwrap().id();
        let c = dir.admit("c", Some(b), s).unwrap().id();

        let mut table = TaskTable::new();
        let top = table.open(None, a, None, chain()).unwrap();
        let mid = table.open(Some(a), b, Some(top), chain()).unwrap();
        let x = table.open(Some(b), c, Some(mid), chain()).unwrap();
        let y = table.open(Some(b), c, Some(mid), chain()).unwrap();

        table.release(top);
        table.release(mid);
        assert_eq!(table.len(), 4, "both still wait on children");
        table.release(x);
        assert!(table.get(x).is_none());
        assert_eq!(table.get(mid).unwrap().outstanding_children, 1);
        table.release(y);
        assert!(table.is_empty(), "the last child closed mid, then top");
        table.release(top);
    }
}
