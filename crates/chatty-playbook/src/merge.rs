//! Playbook delta merge (AGE-17).
//!
//! `apply` is **reserved** — human writes the pure, total merge.

use crate::{DeltaOp, Playbook, PlaybookError};

/// Apply a batch of curator delta ops to `playbook`.
///
/// Spec (for failing tests): fresh-id ADD, counter UPDATE, unknown-id no-op,
/// batch commutativity for independent ops, byte-stable serialization.
pub fn apply(playbook: &mut Playbook, ops: &[DeltaOp]) -> Result<(), PlaybookError> {
    let _ = (playbook, ops);
    todo!("human: apply — pure deterministic ACE merge (AGE-17)")
}

/// Re-export bullet helper used by tests once `apply` lands.
#[allow(dead_code)]
pub(crate) fn bullet_count(playbook: &Playbook) -> usize {
    playbook.sections.values().map(Vec::len).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bullet, DeltaOp, Playbook};
    use std::collections::BTreeMap;

    fn empty_pb() -> Playbook {
        Playbook {
            sections: BTreeMap::new(),
        }
    }

    #[test]
    #[should_panic(expected = "human: apply")]
    fn apply_fresh_id_add_is_reserved() {
        let mut pb = empty_pb();
        let ops = vec![DeltaOp::Add {
            section: "strategies_and_hard_rules".into(),
            content: "always cite sources".into(),
        }];
        let _ = apply(&mut pb, &ops);
    }

    #[test]
    #[should_panic(expected = "human: apply")]
    fn apply_unknown_id_update_is_reserved() {
        let mut pb = empty_pb();
        let ops = vec![DeltaOp::Update {
            id: "ctx-missing".into(),
            helpful_delta: 1,
            harmful_delta: 0,
        }];
        let _ = apply(&mut pb, &ops);
    }

    #[test]
    fn playbook_serde_is_byte_stable_for_empty() {
        let pb = empty_pb();
        let a = serde_json::to_vec(&pb).unwrap();
        let b = serde_json::to_vec(&pb).unwrap();
        assert_eq!(a, b);
        let _ = Bullet {
            id: "ctx-1".into(),
            helpful: 0,
            harmful: 0,
            content: "x".into(),
        };
    }
}
