//! Who is on the fabric: every node a root process's broker admitted.
//!
//! The broker names a node when it admits it; a node never names itself
//! (ADR-0020). Names are `<spec>-<n>` and are unique for the life of the
//! [`Directory`], ended nodes included, so an edge-log row or a transcript
//! line naming `local-coder-3` means one node and only ever that one.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::task_table::RunId;

/// The name the in-process root answers to: the owner of every node admitted
/// with no owner. The root is not a node of the [`Directory`], and no node is
/// ever named this, since every node's name ends in `-<n>`.
pub const ROOT_NAME: &str = "root";

/// A node's broker-local id. Never reused within a root process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(u64);

impl NodeId {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node-{}", self.0)
    }
}

/// A node's name, e.g. `local-coder-3`. Assigned by the broker, never
/// claimed by the node.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct NodeName(String);

impl NodeName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The root conversation a node works for: the unit of a swarm.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ConversationScope(String);

impl ConversationScope {
    pub fn new(root_conversation_id: impl Into<String>) -> Self {
        Self(root_conversation_id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConversationScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a node is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum NodeState {
    /// Admitted; its process or handle is not serving yet.
    Starting,
    /// Serving `run`.
    Running { run: RunId },
    /// Alive with no run.
    Idle,
    /// Gone. Its name stays taken.
    Ended,
}

/// One admitted node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    id: NodeId,
    name: NodeName,
    owner: Option<NodeId>,
    scope: ConversationScope,
    spec: String,
    state: NodeState,
}

impl Node {
    pub fn id(&self) -> NodeId {
        self.id
    }

    pub fn name(&self) -> &NodeName {
        &self.name
    }

    /// The node that asked for this one; `None` for a root.
    pub fn owner(&self) -> Option<NodeId> {
        self.owner
    }

    pub fn scope(&self) -> &ConversationScope {
        &self.scope
    }

    /// The role (virtual agent) this node was started as, e.g. `local-coder`.
    pub fn spec(&self) -> &str {
        &self.spec
    }

    pub fn state(&self) -> NodeState {
        self.state
    }
}

/// Why [`Directory::admit`] refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    #[error("unknown owner {0}")]
    UnknownOwner(NodeId),
    /// A spawn named a caller no node was admitted under.
    #[error("no node is named {0}")]
    UnknownOwnerName(String),
    #[error("owner {owner} works for conversation {owner_scope}, not {requested}")]
    ScopeMismatch {
        owner: NodeId,
        owner_scope: ConversationScope,
        requested: ConversationScope,
    },
    #[error("unknown node {0}")]
    UnknownNode(NodeId),
    /// A node of spec [`ROOT_NAME`]: only the root goes by that name, so
    /// no node may be admitted as it (ADR-0023 § 2).
    #[error("'{ROOT_NAME}' is the root's name; no node is admitted as it")]
    ReservedSpec,
}

/// Every node one root process's broker admitted, ended ones included.
#[derive(Debug, Default)]
pub struct Directory {
    nodes: BTreeMap<NodeId, Node>,
    /// Every name ever issued, ended nodes' included, so none is issued twice.
    by_name: HashMap<String, NodeId>,
    /// Next `<n>` per spec.
    counters: HashMap<String, u64>,
    next_id: u64,
}

impl Directory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit a node started as `spec`, owned by `owner` (`None` for a root),
    /// working for `scope`. The broker names it `<spec>-<n>`.
    ///
    /// An owner must already be in this directory and work for the same
    /// conversation: a swarm never spans two.
    pub fn admit(
        &mut self,
        spec: &str,
        owner: Option<NodeId>,
        scope: ConversationScope,
    ) -> Result<Node, DirectoryError> {
        if spec == ROOT_NAME {
            return Err(DirectoryError::ReservedSpec);
        }
        if let Some(owner) = owner {
            let owner_node = self
                .nodes
                .get(&owner)
                .ok_or(DirectoryError::UnknownOwner(owner))?;
            if owner_node.scope != scope {
                return Err(DirectoryError::ScopeMismatch {
                    owner,
                    owner_scope: owner_node.scope.clone(),
                    requested: scope,
                });
            }
        }

        let counter = self.counters.entry(spec.to_string()).or_insert(0);
        // A spec that itself ends in `-<n>` could produce a name another spec
        // already produced; skip over any name ever issued.
        let name = loop {
            let candidate = format!("{spec}-{counter}");
            *counter += 1;
            if !self.by_name.contains_key(&candidate) {
                break candidate;
            }
        };

        let id = NodeId(self.next_id);
        self.next_id += 1;
        let node = Node {
            id,
            name: NodeName(name),
            owner,
            scope,
            spec: spec.to_string(),
            state: NodeState::Starting,
        };
        self.by_name.insert(node.name.0.clone(), id);
        self.nodes.insert(id, node.clone());
        Ok(node)
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn by_name(&self, name: &str) -> Option<&Node> {
        self.by_name.get(name).and_then(|id| self.nodes.get(id))
    }

    /// Every node working for `scope`, ended ones included, in admission order.
    pub fn in_scope<'a>(
        &'a self,
        scope: &'a ConversationScope,
    ) -> impl Iterator<Item = &'a Node> + 'a {
        self.nodes.values().filter(move |node| &node.scope == scope)
    }

    /// The owner chain of `id`, nearest first, ending at the root. Empty for
    /// a root or an unknown id.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut next = self.nodes.get(&id).and_then(|node| node.owner);
        std::iter::from_fn(move || {
            let current = next?;
            next = self.nodes.get(&current).and_then(|node| node.owner);
            Some(current)
        })
    }

    pub fn set_state(&mut self, id: NodeId, state: NodeState) -> Result<(), DirectoryError> {
        let node = self
            .nodes
            .get_mut(&id)
            .ok_or(DirectoryError::UnknownNode(id))?;
        node.state = state;
        Ok(())
    }

    /// Mark `id` ended. It stays in the directory and its name stays taken.
    pub fn end(&mut self, id: NodeId) -> Result<(), DirectoryError> {
        self.set_state(id, NodeState::Ended)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(id: &str) -> ConversationScope {
        ConversationScope::new(id)
    }

    #[test]
    fn directory_names_are_unique_and_never_reused() {
        let mut dir = Directory::new();
        let root = dir.admit("leader", None, scope("c1")).unwrap();
        let first = dir
            .admit("local-coder", Some(root.id()), scope("c1"))
            .unwrap();
        let second = dir
            .admit("local-coder", Some(root.id()), scope("c1"))
            .unwrap();
        assert_eq!(first.name().as_str(), "local-coder-0");
        assert_eq!(second.name().as_str(), "local-coder-1");

        // Admit, end, admit again: a new name and a new id, and the ended
        // node still answers to its own name.
        dir.end(first.id()).unwrap();
        let third = dir
            .admit("local-coder", Some(root.id()), scope("c1"))
            .unwrap();
        assert_eq!(third.name().as_str(), "local-coder-2");
        assert_ne!(third.id(), first.id());
        let ended = dir.by_name("local-coder-0").unwrap();
        assert_eq!(ended.id(), first.id());
        assert_eq!(ended.state(), NodeState::Ended);

        // A spec that looks like an issued name cannot collide with it.
        let tricky = dir.admit("local-coder-1", None, scope("c2")).unwrap();
        assert_eq!(tricky.name().as_str(), "local-coder-1-0");
        let mut lookalike = Directory::new();
        lookalike.admit("a-0", None, scope("c")).unwrap(); // a-0-0
        lookalike.admit("a", None, scope("c")).unwrap(); // a-0
        let clash = lookalike.admit("a-0", None, scope("c")).unwrap();
        assert_eq!(clash.name().as_str(), "a-0-1");

        let mut names: Vec<String> = [&root, &first, &second, &third, &tricky]
            .iter()
            .map(|n| n.name().to_string())
            .collect();
        let before = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), before, "a name was issued twice: {names:?}");

        // Ids are fresh even across many admissions and ends.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let node = dir.admit("w", None, scope("c3")).unwrap();
            assert!(seen.insert(node.id()), "id {} reused", node.id());
            dir.end(node.id()).unwrap();
        }
    }

    #[test]
    fn directory_ancestors_walks_to_the_root() {
        let mut dir = Directory::new();
        let s = scope("c1");
        let root = dir.admit("leader", None, s.clone()).unwrap();
        let sub = dir
            .admit("local-coordinator", Some(root.id()), s.clone())
            .unwrap();
        let leaf = dir.admit("local-coder", Some(sub.id()), s.clone()).unwrap();
        let sibling = dir
            .admit("local-reviewer", Some(root.id()), s.clone())
            .unwrap();

        assert_eq!(
            dir.ancestors(leaf.id()).collect::<Vec<_>>(),
            vec![sub.id(), root.id()]
        );
        assert_eq!(
            dir.ancestors(sibling.id()).collect::<Vec<_>>(),
            vec![root.id()]
        );
        assert_eq!(dir.ancestors(root.id()).count(), 0);

        // Ending a middle node does not cut the chain.
        dir.end(sub.id()).unwrap();
        assert_eq!(
            dir.ancestors(leaf.id()).collect::<Vec<_>>(),
            vec![sub.id(), root.id()]
        );

        // An owner must exist, so every chain ends at a real root.
        assert_eq!(
            dir.admit("x", Some(NodeId(999)), s.clone()),
            Err(DirectoryError::UnknownOwner(NodeId(999)))
        );
        assert_eq!(dir.ancestors(NodeId(999)).count(), 0);
    }

    #[test]
    fn directory_in_scope_is_per_conversation() {
        let mut dir = Directory::new();
        let a = scope("conversation-a");
        let b = scope("conversation-b");
        let root_a = dir.admit("leader", None, a.clone()).unwrap();
        let root_b = dir.admit("leader", None, b.clone()).unwrap();
        let child_a = dir
            .admit("local-coder", Some(root_a.id()), a.clone())
            .unwrap();
        let child_b = dir
            .admit("local-coder", Some(root_b.id()), b.clone())
            .unwrap();

        fn ids(dir: &Directory, s: &ConversationScope) -> Vec<NodeId> {
            dir.in_scope(s).map(Node::id).collect()
        }
        assert_eq!(ids(&dir, &a), vec![root_a.id(), child_a.id()]);
        assert_eq!(ids(&dir, &b), vec![root_b.id(), child_b.id()]);
        assert_eq!(ids(&dir, &scope("conversation-c")), Vec::<NodeId>::new());

        // A node cannot be owned across conversations.
        let err = dir
            .admit("local-coder", Some(root_a.id()), b.clone())
            .unwrap_err();
        assert!(matches!(err, DirectoryError::ScopeMismatch { .. }));
        assert_eq!(ids(&dir, &b).len(), 2);
    }

    /// ADR-0023 § 2: no node may take the root's name; the root's chain
    /// element stays its own.
    #[test]
    fn no_node_is_admitted_as_the_root() {
        let mut dir = Directory::new();
        assert!(matches!(
            dir.admit(ROOT_NAME, None, scope("c")),
            Err(DirectoryError::ReservedSpec)
        ));
        assert!(dir.by_name("root-0").is_none());
    }
}
