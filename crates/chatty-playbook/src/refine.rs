//! Grow-and-refine (AGE-17).
//!
//! `grow_and_refine` is **reserved** — human writes de-dup + pruning caps.

use crate::{Playbook, PlaybookError};

/// Collapse near-duplicate bullets and enforce configured size caps.
///
/// Spec (for failing tests): near-duplicates above the similarity threshold
/// collapse to one; pruning respects the configured token/bullet cap.
pub fn grow_and_refine(
    playbook: &mut Playbook,
    similarity_threshold: f32,
    max_bullets: usize,
) -> Result<(), PlaybookError> {
    let _ = (playbook, similarity_threshold, max_bullets);
    todo!("human: grow_and_refine — de-dup + bounded growth (AGE-17)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Playbook;
    use std::collections::BTreeMap;

    #[test]
    #[should_panic(expected = "human: grow_and_refine")]
    fn near_duplicate_collapse_is_reserved() {
        let mut pb = Playbook {
            sections: BTreeMap::new(),
        };
        let _ = grow_and_refine(&mut pb, 0.8, 100);
    }
}
