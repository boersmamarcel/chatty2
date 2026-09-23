//! Passage selection for `search_web` (AGE-517, M6 "reranking" stage): split
//! fetched page text into overlapping word windows and score them with BM25
//! against the query, so the snippet the model reads is the part of the page
//! that matches, not whatever the engine chose to show.
//!
//! Corpus statistics (idf, average length) come from the passages of the
//! pages fetched for *this* query — a tiny, per-query collection. That makes
//! idf coarse, but it needs no index and no background corpus.

use std::collections::{HashMap, HashSet};

/// Words per passage and the step between passage starts (50% overlap), so a
/// sentence cut by one window boundary is whole in the next.
pub const PASSAGE_WORDS: usize = 60;
pub const PASSAGE_STRIDE: usize = 30;

const K1: f64 = 1.2;
const B: f64 = 0.75;

/// Lowercased alphanumeric runs.
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Overlapping windows of `words` words, `stride` apart; the last window is
/// kept even when shorter. Empty text gives no passages.
pub fn split_passages(text: &str, words: usize, stride: usize) -> Vec<String> {
    let all: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    let mut start = 0;
    while start < all.len() {
        let end = (start + words).min(all.len());
        out.push(all[start..end].join(" "));
        if end == all.len() {
            break;
        }
        start += stride.max(1);
    }
    out
}

/// BM25 score of each passage for `query_terms` (deduplicated), with idf
/// `ln((N − df + 0.5)/(df + 0.5) + 1)` and length normalisation over the
/// given passages.
pub fn score_passages(query_terms: &[String], passages: &[Vec<String>]) -> Vec<f64> {
    let n = passages.len() as f64;
    if passages.is_empty() {
        return Vec::new();
    }
    let avgdl = passages.iter().map(Vec::len).sum::<usize>() as f64 / n;
    let terms: HashSet<&str> = query_terms.iter().map(String::as_str).collect();
    let df: HashMap<&str, usize> = terms
        .iter()
        .map(|t| {
            (
                *t,
                passages.iter().filter(|p| p.iter().any(|w| w == t)).count(),
            )
        })
        .collect();
    passages
        .iter()
        .map(|p| {
            let len = p.len() as f64;
            terms
                .iter()
                .map(|t| {
                    let tf = p.iter().filter(|w| w == t).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let d = df[t] as f64;
                    let idf = ((n - d + 0.5) / (d + 0.5) + 1.0).ln();
                    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avgdl.max(1.0)))
                })
                .sum()
        })
        .collect()
}

/// Remove the ` (https://…)` link targets `fetch_tool::html_to_text` keeps
/// after anchor text: they are noise for term matching.
pub fn strip_link_targets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(" (http") {
        out.push_str(&rest[..i]);
        match rest[i..].find(')') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_overlap_and_keep_the_tail() {
        let text = (1..=10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            split_passages(&text, 4, 2),
            vec!["1 2 3 4", "3 4 5 6", "5 6 7 8", "7 8 9 10"]
        );
        assert_eq!(split_passages("a b", 4, 2), vec!["a b"]);
        assert!(split_passages("  ", 4, 2).is_empty());
    }

    #[test]
    fn bm25_prefers_rare_matching_terms_and_shorter_passages() {
        let q = tokenize("Jerlov Award 2018");
        let passages: Vec<Vec<String>> = [
            "the award was given in many years to many people",
            "Annick Bricaud received the Jerlov Award in 2018",
            "the award the award the award the award",
        ]
        .iter()
        .map(|p| tokenize(p))
        .collect();
        let s = score_passages(&q, &passages);
        assert!(s[1] > s[0] && s[1] > s[2], "{s:?}");
        // tf saturates: repeating "award" does not beat a rare-term match.
        assert!(s[2] < s[1]);
        assert!(score_passages(&q, &[]).is_empty());
    }

    #[test]
    fn non_matching_passage_scores_zero() {
        let s = score_passages(&tokenize("zebra"), &[tokenize("no match here")]);
        assert_eq!(s, vec![0.0]);
    }

    #[test]
    fn strips_link_targets() {
        assert_eq!(
            strip_link_targets("See Paris (https://en.wikipedia.org/wiki/Paris) and more"),
            "See Paris and more"
        );
        assert_eq!(strip_link_targets("no links"), "no links");
    }
}
