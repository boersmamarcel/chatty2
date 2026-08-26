//! AFlow soft-mixed parent selection (AGE-13).

use crate::OptimizeError;

/// Soft mixed probability over top-k plus the blank template.
///
/// Paper: `P(i) = λ·(1/n) + (1−λ)·softmax(α·(s_i − s_max))` with λ=0.2, α=0.4.
pub fn soft_mixed_select(scores: &[f64], lambda: f64, alpha: f64) -> Result<usize, OptimizeError> {
    let _ = (scores, lambda, alpha);
    todo!("human: soft_mixed_select — λ=0.2 α=0.4 + blank-template guarantee (AGE-13)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "human: soft_mixed_select")]
    fn soft_mixed_select_is_reserved() {
        let _ = soft_mixed_select(&[0.1, 0.9, 0.5], 0.2, 0.4);
    }
}
