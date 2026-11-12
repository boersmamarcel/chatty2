//! The relay counter: the F3 gate's instrument (TM-3).
//!
//! F3 (`team`: sideways messages) is built only if enough multi-worker runs
//! contain a *relay request*: a child's message to its owner that names one
//! of its siblings. The broker reads each such message against its
//! [`Directory`] and writes the answer on the message's edge-log row;
//! `scripts/fabric/relay-rate.sh` turns the rows into the rate.

use crate::directory::{Directory, Node, NodeId};

/// Reads a message body for the siblings it names.
pub struct RelayCounter;

impl RelayCounter {
    /// The broker-assigned names of `sender`'s siblings that `body` names,
    /// in admission order. A sibling is a node with the same owner in the
    /// same scope, kept only where `in_run` says so; the sender is not its own sibling.
    ///
    /// `in_run` limits siblings to the sender's own run: the Directory keeps
    /// ended nodes, so without it a finished sibling of an earlier run under
    /// the same owner would count.
    ///
    /// A sibling is named by its broker-assigned name (`reviewer-0`) or by
    /// its role, its spec name (`reviewer`), as a whole token, ignoring
    /// case. Leaf workers never see broker-assigned names, so the role
    /// must count or the gate would read low. A role name written as a
    /// common noun ("the reviewer said") counts too. The exception is the
    /// sender's *own* spec name: two `local-coder` siblings share it and a
    /// coder may name itself, so it never counts on its own; a sibling of
    /// that spec counts only under its broker-assigned name.
    pub fn siblings_named(
        directory: &Directory,
        sender: NodeId,
        body: &str,
        in_run: impl Fn(&Node) -> bool,
    ) -> Vec<String> {
        let Some(me) = directory.get(sender) else {
            return Vec::new();
        };
        let body = body.to_lowercase();
        directory
            .in_scope(me.scope())
            .filter(|node| node.id() != me.id() && node.owner() == me.owner())
            .filter(|node| in_run(node))
            .filter(|node| {
                names_token(&body, node.name().as_str())
                    || (node.spec() != me.spec() && names_token(&body, node.spec()))
            })
            .map(|node| node.name().to_string())
            .collect()
    }
}

/// Whether the lowercase `body` holds `name` as a whole token: not run on
/// into more name characters (`reviewer-10` is not `reviewer-1`; `coder` is
/// not in `local-coder`).
fn names_token(body: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    let is_name_char = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    body.match_indices(&name).any(|(at, _)| {
        let before = body[..at].chars().next_back();
        let after = body[at + name.len()..].chars().next();
        !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::ConversationScope;

    struct Tree {
        dir: Directory,
        coder0: NodeId,
        coder1: NodeId,
        reviewer: NodeId,
        coder: NodeId,
    }

    /// leader owns local-coder-0, local-coder-1, coder-0 and reviewer-0; a stranger
    /// `other-0` is owned by another leader in another scope.
    fn tree() -> Tree {
        let mut dir = Directory::new();
        let scope = ConversationScope::new("c");
        let leader = dir.admit("leader", None, scope.clone()).unwrap();
        let coder0 = dir
            .admit("local-coder", Some(leader.id()), scope.clone())
            .unwrap();
        let coder1 = dir
            .admit("local-coder", Some(leader.id()), scope.clone())
            .unwrap();
        let coder = dir
            .admit("coder", Some(leader.id()), scope.clone())
            .unwrap();
        let reviewer = dir.admit("reviewer", Some(leader.id()), scope).unwrap();
        let other_scope = ConversationScope::new("d");
        let other = dir.admit("leader", None, other_scope.clone()).unwrap();
        dir.admit("planner", Some(other.id()), other_scope).unwrap();
        Tree {
            coder0: coder0.id(),
            coder1: coder1.id(),
            reviewer: reviewer.id(),
            coder: coder.id(),
            dir,
        }
    }

    fn named(t: &Tree, sender: NodeId, body: &str) -> Vec<String> {
        RelayCounter::siblings_named(&t.dir, sender, body, |_| true)
    }

    #[test]
    fn relay_counter_counts_a_sibling_by_either_name() {
        let t = tree();
        assert_eq!(
            named(&t, t.coder0, "please ask local-coder-1 to rebase"),
            ["local-coder-1"]
        );
        assert_eq!(
            named(&t, t.coder0, "tell reviewer-0 it is ready."),
            ["reviewer-0"]
        );
        // By role name, and as a common noun.
        assert_eq!(
            named(&t, t.coder0, "Can the Reviewer look at this?"),
            ["reviewer-0"]
        );
        assert_eq!(
            named(&t, t.reviewer, "the local-coder said it builds"),
            ["local-coder-0", "local-coder-1"]
        );
    }

    #[test]
    fn relay_counter_ignores_no_one_and_non_siblings() {
        let t = tree();
        assert!(named(&t, t.coder0, "done, tests pass").is_empty());
        // The leader (owner) and another scope's nodes are not siblings.
        assert!(named(&t, t.coder0, "leader-0 and planner-0 and planner").is_empty());
    }

    #[test]
    fn relay_counter_needs_whole_tokens() {
        let t = tree();
        assert!(named(&t, t.coder0, "local-coder-10 is slow").is_empty());
        assert!(named(&t, t.coder0, "prereviewer-0 and reviewer-00").is_empty());
        assert!(named(&t, t.coder0, "the reviewers disagree").is_empty());
        assert!(named(&t, t.reviewer, "my local-coders").is_empty());
        // `coder-0` is a real sibling: its role is not inside `local-coder`,
        // and `local-coder-1` is not `coder-0`.
        assert!(
            named(&t, t.reviewer, "local-coder-1 is done")
                .iter()
                .all(|n| n != "coder-0")
        );
        assert_eq!(named(&t, t.reviewer, "a coder wrote it"), ["coder-0"]);
        assert!(
            named(&t, t.reviewer, "my local-coder-1")
                .iter()
                .all(|n| n != "coder-0")
        );
    }

    #[test]
    fn relay_counter_ignores_the_senders_own_spec_name() {
        let t = tree();
        // Two local-coders share the spec: naming it names no one in
        // particular, and a coder may name itself.
        assert!(named(&t, t.coder0, "as a local-coder I changed it").is_empty());
        assert!(named(&t, t.coder0, "local-coder-0 is done").is_empty());
        // A distinct broker-assigned sibling name still counts.
        assert_eq!(
            named(&t, t.coder0, "as a local-coder I agree with local-coder-1"),
            ["local-coder-1"]
        );
        assert_eq!(
            named(&t, t.coder1, "local-coder-0 first"),
            ["local-coder-0"]
        );
    }

    #[test]
    fn relay_counter_ignores_siblings_outside_the_run() {
        let t = tree();
        // The reviewer ended in an earlier run under the same owner: the
        // Directory still has it, but this run has none.
        let in_run = |node: &Node| node.name().as_str() != "reviewer-0";
        let named = RelayCounter::siblings_named(&t.dir, t.coder, "the reviewer said", in_run);
        assert!(named.is_empty());
        let named = RelayCounter::siblings_named(&t.dir, t.coder, "ask local-coder-0", in_run);
        assert_eq!(named, ["local-coder-0"]);
    }
}
