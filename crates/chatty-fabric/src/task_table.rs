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
//!
//! The runs of one root form a tree (ADR-0092): every run records the run
//! it was started from, and has exactly one. A cancel or a deadline on a
//! run goes to its whole subtree, deepest runs first:
//! [`TaskTable::cancel_subtree`] marks the subtree so none of it can start
//! another run, and hands the broker the runs to stop, level by level.

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
    /// This run, or a run above it, is being cancelled
    /// ([`TaskTable::cancel_subtree`]): no run may start from it.
    #[serde(default)]
    pub cancelling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TaskTableError {
    #[error("unknown run {0}")]
    UnknownRun(RunId),
    #[error("{0} still has {1} outstanding child run(s)")]
    ChildrenOutstanding(RunId, u32),
    #[error("{0} is being cancelled: no run may start from it")]
    Cancelling(RunId),
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
    /// outstanding child until this run closes. A parent being cancelled
    /// starts nothing more.
    pub fn open(
        &mut self,
        caller: Option<NodeId>,
        callee: NodeId,
        parent: Option<RunId>,
        chain: CallChain,
    ) -> Result<RunId, TaskTableError> {
        if let Some(parent) = parent {
            let entry = self
                .runs
                .get_mut(&parent)
                .ok_or(TaskTableError::UnknownRun(parent))?;
            if entry.cancelling {
                return Err(TaskTableError::Cancelling(parent));
            }
            entry.outstanding_children += 1;
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
                cancelling: false,
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

    /// The open runs started from `run`, in the order they were opened.
    pub fn children(&self, run: RunId) -> impl Iterator<Item = RunId> + '_ {
        self.runs
            .iter()
            .filter(move |(_, entry)| entry.parent == Some(run))
            .map(|(id, _)| *id)
    }

    /// The open runs under `run`, by level: its children first, then their
    /// children, and so on. `run` itself is not in it.
    pub fn descendants(&self, run: RunId) -> Vec<Vec<RunId>> {
        let mut levels = Vec::new();
        let mut level: Vec<RunId> = self.children(run).collect();
        while !level.is_empty() {
            let next = level.iter().flat_map(|id| self.children(*id)).collect();
            levels.push(std::mem::replace(&mut level, next));
        }
        levels
    }

    /// The runs above `run`, nearest first, up to its root run.
    pub fn ancestors(&self, run: RunId) -> Vec<RunId> {
        let mut ancestors = Vec::new();
        let mut at = self.runs.get(&run).and_then(|entry| entry.parent);
        while let Some(id) = at {
            ancestors.push(id);
            at = self.runs.get(&id).and_then(|entry| entry.parent);
        }
        ancestors
    }

    /// Start cancelling `run` and everything under it (ADR-0092): every run
    /// in the subtree is marked, so none of them can start another run, and
    /// the runs under `run` come back deepest level first — the order the
    /// broker stops them in, each level before the one above it. An unknown
    /// run has nothing under it.
    pub fn cancel_subtree(&mut self, run: RunId) -> Vec<Vec<RunId>> {
        let Some(entry) = self.runs.get_mut(&run) else {
            return Vec::new();
        };
        entry.cancelling = true;
        let mut levels = self.descendants(run);
        for id in levels.iter().flatten() {
            if let Some(entry) = self.runs.get_mut(id) {
                entry.cancelling = true;
            }
        }
        levels.reverse();
        levels
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

    /// ADR-0092, T1 "task tree": every run records its parent, and the
    /// tree of a known shape is found again from any run in it. The shape
    /// is the diamond of the test plan's T2.7 at depth 3: a research run
    /// with two coding runs, each with a coder, then a reviewer run.
    #[test]
    fn the_runs_of_a_root_form_a_tree() {
        let mut dir = Directory::new();
        let s = ConversationScope::new("c");
        let research = dir.admit("research", None, s.clone()).unwrap().id();
        let coding_a = dir.admit("coding", Some(research), s.clone()).unwrap().id();
        let coding_b = dir.admit("coding", Some(research), s.clone()).unwrap().id();
        let coder_a = dir.admit("coder", Some(coding_a), s.clone()).unwrap().id();
        let coder_b = dir.admit("coder", Some(coding_b), s.clone()).unwrap().id();
        let reviewer = dir.admit("reviewer", Some(research), s).unwrap().id();

        let mut table = TaskTable::new();
        let top = table.open(None, research, None, chain()).unwrap();
        let a = table
            .open(Some(research), coding_a, Some(top), chain())
            .unwrap();
        let b = table
            .open(Some(research), coding_b, Some(top), chain())
            .unwrap();
        let a1 = table
            .open(Some(coding_a), coder_a, Some(a), chain())
            .unwrap();
        let b1 = table
            .open(Some(coding_b), coder_b, Some(b), chain())
            .unwrap();
        let r = table
            .open(Some(research), reviewer, Some(top), chain())
            .unwrap();

        assert_eq!(table.get(top).unwrap().parent, None);
        for (run, parent) in [(a, top), (b, top), (r, top), (a1, a), (b1, b)] {
            assert_eq!(table.get(run).unwrap().parent, Some(parent), "{run}");
        }
        assert_eq!(table.children(top).collect::<Vec<_>>(), [a, b, r]);
        assert_eq!(table.children(a).collect::<Vec<_>>(), [a1]);
        assert_eq!(table.children(a1).count(), 0);
        assert_eq!(table.descendants(top), [vec![a, b, r], vec![a1, b1]]);
        assert_eq!(table.descendants(b), [vec![b1]]);
        assert!(table.descendants(r).is_empty());
        assert_eq!(table.ancestors(b1), [b, top]);
        assert!(table.ancestors(top).is_empty());

        // A closed run leaves the tree; its siblings stay.
        table.release(r);
        assert_eq!(table.descendants(top), [vec![a, b], vec![a1, b1]]);
    }

    /// ADR-0092: a cancel marks the whole subtree, so nothing in it can
    /// start another run, and hands the runs under it back deepest level
    /// first. Runs outside the subtree are untouched.
    #[test]
    fn a_cancel_marks_the_subtree_and_stops_it_bottom_up() {
        let mut dir = Directory::new();
        let s = ConversationScope::new("c");
        let n: Vec<_> = (0..5)
            .map(|i| dir.admit(&format!("n{i}"), None, s.clone()).unwrap().id())
            .collect();

        let mut table = TaskTable::new();
        let top = table.open(None, n[0], None, chain()).unwrap();
        let mid = table.open(Some(n[0]), n[1], Some(top), chain()).unwrap();
        let leaf = table.open(Some(n[1]), n[2], Some(mid), chain()).unwrap();
        let other = table.open(Some(n[0]), n[3], Some(top), chain()).unwrap();
        let elsewhere = table.open(None, n[4], None, chain()).unwrap();

        assert_eq!(table.cancel_subtree(mid), [vec![leaf]]);
        assert!(table.get(mid).unwrap().cancelling);
        assert!(table.get(leaf).unwrap().cancelling);
        assert!(!table.get(top).unwrap().cancelling, "the caller carries on");
        assert!(!table.get(other).unwrap().cancelling);
        for parent in [mid, leaf] {
            assert_eq!(
                table.open(Some(n[2]), n[2], Some(parent), chain()),
                Err(TaskTableError::Cancelling(parent)),
            );
        }
        let beside = table.open(Some(n[3]), n[4], Some(other), chain()).unwrap();

        let levels = table.cancel_subtree(top);
        assert_eq!(
            levels,
            [vec![leaf, beside], vec![mid, other]],
            "deepest first"
        );
        assert!(table.get(beside).unwrap().cancelling);
        assert!(!table.get(elsewhere).unwrap().cancelling, "another root");
        assert!(table.cancel_subtree(RunId(999)).is_empty());

        // The subtree closes bottom-up as each stopped call ends.
        table.release(top);
        for run in levels.concat() {
            table.release(run);
        }
        assert_eq!(table.len(), 1, "only the other root's run is left");
    }
}
