//! Optimizer archive / selection strategy trait (AGE-7).
//!
//! `SelectionStrategy` is **reserved**.

use crate::OptimizeError;

/// Shared selection seam for AFlow, GEPA, and (externally) DGM.
pub trait SelectionStrategy {
    fn select(&self, scores: &[f64]) -> Result<usize, OptimizeError> {
        let _ = scores;
        todo!("human: SelectionStrategy — shape AFlow/GEPA/DGM instantiate (AGE-7)")
    }
}

/// Placeholder concrete type so the module compiles; methods stay reserved.
pub struct UnimplementedSelection;

impl SelectionStrategy for UnimplementedSelection {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: SelectionStrategy")]
    fn selection_strategy_is_reserved() {
        let s = UnimplementedSelection;
        let _ = s.select(&[1.0, 2.0]);
    }
}
