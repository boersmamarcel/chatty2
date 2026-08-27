//! HotpotQA helpers for the walking skeleton (AGE-22).
//!
//! Pure metric helpers only — wire into human-written [`FeedbackFn`](chatty_trace::FeedbackFn).

use crate::datasets::HotpotQaItem;

/// Normalized exact match (lower case, strip punctuation) for acceptance gating.
pub fn normalized_exact_match(prediction: &str, gold: &str) -> bool {
    normalize(prediction) == normalize(gold)
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Gold supporting titles not yet present in `retrieved` (case-insensitive substring match).
///
/// Intended for per-hop feedback once `FeedbackFn` is human-written.
pub fn missing_supporting_titles(retrieved: &[String], gold: &[String]) -> Vec<String> {
    gold.iter()
        .filter(|title| {
            let t = title.to_lowercase();
            !retrieved
                .iter()
                .any(|r| r.to_lowercase().contains(&t) || t.contains(&r.to_lowercase()))
        })
        .cloned()
        .collect()
}

/// Pick the first `n` items after a seeded shuffle (deterministic slice for skeleton runs).
pub fn select_items(items: &[HotpotQaItem], seed: u64, n: usize) -> Vec<HotpotQaItem> {
    use rand::SeedableRng;
    use rand::seq::SliceRandom;
    use rand_chacha::ChaCha8Rng;

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut out: Vec<HotpotQaItem> = items.to_vec();
    out.shuffle(&mut rng);
    out.truncate(n.min(out.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn em_is_normalized() {
        assert!(normalized_exact_match("Paris.", "paris"));
        assert!(!normalized_exact_match("London", "paris"));
    }

    #[test]
    fn missing_titles_detects_gap() {
        let gold = vec!["France".into(), "Eiffel Tower".into()];
        let retrieved = vec!["Eiffel Tower article".into()];
        let missing = missing_supporting_titles(&retrieved, &gold);
        assert_eq!(missing, vec!["France".to_string()]);
    }

    #[test]
    fn select_items_is_seeded() {
        let items: Vec<HotpotQaItem> = (0..10)
            .map(|i| HotpotQaItem {
                id: format!("hp-{i}"),
                question: format!("q{i}"),
                answer: format!("a{i}"),
                supporting_titles: vec![],
            })
            .collect();
        let a = select_items(&items, 99, 5);
        let b = select_items(&items, 99, 5);
        assert_eq!(a, b);
        assert_eq!(a.len(), 5);
    }
}
