//! Which runs are in flight between which nodes.
//!
//! A run is one task a caller handed a callee. When a callee delegates in
//! turn, its run gains an outstanding child; the per-run endpoint permits of
//! BI-6 release a run's permit while that count is above zero.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEntry {
    pub caller: NodeId,
    pub callee: NodeId,
    /// The caller's run this one was started from; `None` for a root task.
    pub parent: Option<RunId>,
    /// The delegation chain, filled by the delegation policy (PL-S2).
    pub chain: Vec<String>,
    /// Child runs started from this one that have not closed yet.
    pub outstanding_children: u32,
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
        caller: NodeId,
        callee: NodeId,
        parent: Option<RunId>,
        chain: Vec<String>,
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
        if let Some(parent) = entry.parent
            && let Some(parent) = self.runs.get_mut(&parent)
        {
            parent.outstanding_children -= 1;
        }
        Ok(entry)
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

    #[test]
    fn a_child_run_is_outstanding_on_its_parent_until_it_closes() {
        let mut dir = Directory::new();
        let s = ConversationScope::new("c");
        let root = dir.admit("leader", None, s.clone()).unwrap().id();
        let sub = dir.admit("sub", Some(root), s.clone()).unwrap().id();
        let leaf = dir.admit("leaf", Some(sub), s).unwrap().id();

        let mut table = TaskTable::new();
        let top = table.open(root, sub, None, vec![]).unwrap();
        let a = table
            .open(sub, leaf, Some(top), vec!["sub".into()])
            .unwrap();
        let b = table
            .open(sub, leaf, Some(top), vec!["sub".into()])
            .unwrap();
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
        assert_eq!((entry.caller, entry.callee), (root, sub));
        assert!(table.is_empty());

        // Ids are not reused after a close.
        let next = table.open(root, sub, None, vec![]).unwrap();
        assert!(next > b);
        assert_eq!(
            table.open(root, sub, Some(top), vec![]),
            Err(TaskTableError::UnknownRun(top))
        );
    }
}
