//! Tool-level retrieval scoring for `search_web` (AGE-515 SimpleQA, AGE-516
//! FRAMES). Pure: the runner (`chatty-core/examples/search_eval.rs`) calls the
//! real tool and writes one [`SearchEvalRow`] per item; everything here turns
//! rows into numbers, so a run can be rescored (e.g. without Wikipedia)
//! without calling the network again.
//!
//! **Frozen once baselines are recorded.** Normalization, the leak blocklist
//! and the hit rules define what the numbers mean; changing them to raise a
//! score invalidates every earlier run. If one is genuinely wrong, fix it and
//! re-run all baselines.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use unicode_normalization::UnicodeNormalization;

// ── Rows ────────────────────────────────────────────────────────────────────

/// One result as returned by the tool, with the backend that produced it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// `tavily`, `brave`, `bing`, `duckduckgo`, `wikipedia`, `web_browser`, …
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Ok,
    Error,
    Empty,
}

/// Bucket for a failed call, from the tool's error text.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    RateLimit,
    CreditsQuota,
    Timeout,
    Server5xx,
    Parse,
    BotChallengeDecoy,
    /// `--replay` found no recorded response for a request.
    ReplayMiss,
    Other,
}

/// One JSONL row: everything needed to rescore an item later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchEvalRow {
    pub id: String,
    pub dataset: String,
    pub provider: String,
    /// `single` or `fanout`.
    pub mode: String,
    pub queries: Vec<String>,
    pub status: CallStatus,
    #[serde(default)]
    pub error_class: Option<ErrorClass>,
    #[serde(default)]
    pub error_text: Option<String>,
    pub gold_answer: String,
    /// FRAMES gold Wikipedia links; empty for SimpleQA.
    #[serde(default)]
    pub gold_links: Vec<String>,
    /// SimpleQA topic or FRAMES reasoning types.
    #[serde(default)]
    pub strata: Vec<String>,
    pub results: Vec<EvalResult>,
    pub latency_ms: u64,
    pub n_calls: usize,
    pub bytes: usize,
    /// Whether any call escalated to the browser (keyless tier).
    #[serde(default)]
    pub escalated: bool,
    #[serde(default)]
    pub replayed: bool,
    /// Scores at write time (no exclusions), for reading the JSONL by eye.
    #[serde(default)]
    pub score: Option<RowScore>,
}

// ── Normalization and matching ──────────────────────────────────────────────

/// NFKC, lowercase, `1,000` → `1000`, punctuation to spaces, drop the
/// articles a/an/the, collapse whitespace.
pub fn normalize_answer(s: &str) -> String {
    let s: String = s.nfkc().collect::<String>().to_lowercase();
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        let digit_comma = c == ','
            && i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        if digit_comma {
            continue;
        }
        out.push(if c.is_alphanumeric() { c } else { ' ' });
    }
    out.split_whitespace()
        .filter(|w| !matches!(*w, "a" | "an" | "the"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the normalized `answer` occurs as a whole-token run in `text`.
pub fn contains_answer(text: &str, answer: &str) -> bool {
    let answer = normalize_answer(answer);
    if answer.is_empty() {
        return false;
    }
    format!(" {} ", normalize_answer(text)).contains(&format!(" {answer} "))
}

/// Domains that republish SimpleQA/FRAMES with their answers. A leaked
/// result occupies its rank but never counts as a hit.
pub const LEAK_BLOCKLIST: &[&str] = &[
    "huggingface.co",
    "kaggle.com",
    "openaipublic.blob.core.windows.net",
    "github.com/openai/simple-evals",
    "raw.githubusercontent.com/openai/simple-evals",
    "github.com/google-research/frames",
    "paperswithcode.com",
];

pub fn is_leak(url: &str) -> bool {
    let lower = url.to_lowercase();
    let rest = lower.split_once("://").map_or(lower.as_str(), |(_, r)| r);
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    LEAK_BLOCKLIST.iter().any(|d| {
        rest.starts_with(d)
            && rest[d.len()..]
                .chars()
                .next()
                .is_none_or(|c| matches!(c, '/' | '?' | '#' | ':'))
            || (!d.contains('/') && host_of(rest).ends_with(&format!(".{d}")))
    })
}

fn host_of(rest: &str) -> &str {
    rest.split(['/', '?', '#', ':']).next().unwrap_or(rest)
}

/// Canonical form of a Wikipedia URL for gold-link matching: lowercase host,
/// mobile → desktop, no scheme/query/fragment/trailing slash, percent-decoded
/// title with spaces as `_`. Non-Wikipedia URLs pass through lowercased.
pub fn normalize_wiki_url(url: &str) -> String {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['#', '?']).next().unwrap_or(rest);
    let rest = rest.trim_end_matches('/');
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host
        .to_lowercase()
        .replace(".m.wikipedia.org", ".wikipedia.org");
    if !host.ends_with("wikipedia.org") {
        return format!("{host}/{path}").to_lowercase();
    }
    format!("{host}/{}", percent_decode(path).replace(' ', "_"))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Bucket a tool error message.
pub fn classify_error(text: &str) -> ErrorClass {
    let t = text.to_lowercase();
    let has_status = |code: &str| {
        t.contains(&format!(" {code}"))
            || t.contains(&format!("http {code}"))
            || t.contains(&format!("{code} "))
    };
    if t.contains("replay miss") {
        ErrorClass::ReplayMiss
    } else if has_status("429") || t.contains("rate limit") || t.contains("too many requests") {
        ErrorClass::RateLimit
    } else if has_status("432")
        || has_status("433")
        || t.contains("quota")
        || t.contains("credit")
        || t.contains("usage limit")
        || t.contains("plan limit")
    {
        ErrorClass::CreditsQuota
    } else if t.contains("timed out") || t.contains("timeout") || t.contains("deadline") {
        ErrorClass::Timeout
    } else if (500..600).any(|c| has_status(&c.to_string())) {
        ErrorClass::Server5xx
    } else if t.contains("challenge")
        || t.contains("decoy")
        || t.contains("anomaly")
        || t.contains("captcha")
        || t.contains("unrelated to the query")
    {
        ErrorClass::BotChallengeDecoy
    } else if t.contains("parse") || t.contains("decode") || t.contains("invalid json") {
        ErrorClass::Parse
    } else {
        ErrorClass::Other
    }
}

// ── Per-row scoring ─────────────────────────────────────────────────────────

pub const HIT_KS: [usize; 3] = [1, 3, 5];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RowScore {
    /// `hit@1`, `hit@3`, `hit@5`: gold answer in title+snippet of a
    /// non-leaked result ranked ≤ k.
    pub hit: [bool; 3],
    /// A leaked result ranked ≤ 5 contained the answer (reported, not a hit).
    pub leak_hit: bool,
    /// FRAMES: matched gold links / all gold links at k = 5 and 10.
    pub source_recall_5: f64,
    pub source_recall_10: f64,
    pub all_sources_5: bool,
    pub all_sources_10: bool,
}

/// Drop results whose `source` is in `exclude`, keeping order (the remaining
/// results move up, as if that backend had not been queried).
pub fn without_sources(results: &[EvalResult], exclude: &[String]) -> Vec<EvalResult> {
    results
        .iter()
        .filter(|r| !exclude.iter().any(|e| e.eq_ignore_ascii_case(&r.source)))
        .cloned()
        .collect()
}

pub fn answer_hit_at_k(answer: &str, results: &[EvalResult], k: usize) -> bool {
    results
        .iter()
        .take(k)
        .any(|r| !is_leak(&r.url) && contains_answer(&format!("{} {}", r.title, r.snippet), answer))
}

/// Matched gold links among the first `k` results, and the gold total.
pub fn source_matches_at_k(gold: &[String], results: &[EvalResult], k: usize) -> (usize, usize) {
    let got: Vec<String> = results
        .iter()
        .take(k)
        .map(|r| normalize_wiki_url(&r.url))
        .collect();
    let mut gold_norm: Vec<String> = gold.iter().map(|g| normalize_wiki_url(g)).collect();
    gold_norm.sort();
    gold_norm.dedup();
    let matched = gold_norm.iter().filter(|g| got.contains(g)).count();
    (matched, gold_norm.len())
}

pub fn score_results(gold_answer: &str, gold_links: &[String], results: &[EvalResult]) -> RowScore {
    let hit = HIT_KS.map(|k| answer_hit_at_k(gold_answer, results, k));
    let leak_hit = results.iter().take(5).any(|r| {
        is_leak(&r.url) && contains_answer(&format!("{} {}", r.title, r.snippet), gold_answer)
    });
    let recall = |k| {
        let (m, n) = source_matches_at_k(gold_links, results, k);
        (
            if n == 0 { 0.0 } else { m as f64 / n as f64 },
            n > 0 && m == n,
        )
    };
    let (source_recall_5, all_sources_5) = recall(5);
    let (source_recall_10, all_sources_10) = recall(10);
    RowScore {
        hit,
        leak_hit,
        source_recall_5,
        source_recall_10,
        all_sources_5,
        all_sources_10,
    }
}

/// Status and score of a row after dropping `exclude` sources. A row whose
/// every result came from an excluded source becomes `Empty`.
pub fn rescore(row: &SearchEvalRow, exclude: &[String]) -> (CallStatus, RowScore) {
    let results = without_sources(&row.results, exclude);
    let status = match row.status {
        CallStatus::Error => CallStatus::Error,
        _ if results.is_empty() => CallStatus::Empty,
        s => s,
    };
    (
        status,
        score_results(&row.gold_answer, &row.gold_links, &results),
    )
}

// ── Summaries ───────────────────────────────────────────────────────────────

/// Wilson score interval (95%) for `k` successes out of `n`.
pub fn wilson_ci(k: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 0.0);
    }
    let z = 1.959_963_985_f64;
    let n_f = n as f64;
    let p = k as f64 / n_f;
    let denom = 1.0 + z * z / n_f;
    let centre = (p + z * z / (2.0 * n_f)) / denom;
    let half = z * ((p * (1.0 - p) / n_f + z * z / (4.0 * n_f * n_f)).sqrt()) / denom;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// Nearest-rank percentile (`q` in 0..=100); 0 for an empty slice.
pub fn percentile(values: &[u64], q: f64) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let rank = ((q / 100.0) * v.len() as f64).ceil().max(1.0) as usize;
    v[rank.min(v.len()) - 1]
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub n: usize,
    pub errors: usize,
    pub empty: usize,
    pub hits: [usize; 3],
    pub leak_hits: usize,
    pub source_recall_5: f64,
    pub source_recall_10: f64,
    pub all_sources_5: usize,
    pub all_sources_10: usize,
    pub latencies: Vec<u64>,
    pub latencies_fast: Vec<u64>,
    pub latencies_escalated: Vec<u64>,
    pub error_classes: BTreeMap<ErrorClass, usize>,
    /// Results contributed per `source`, and hits@5 whose first hitting
    /// result came from that source.
    pub by_source: BTreeMap<String, (usize, usize)>,
    pub n_calls: usize,
    pub n_results: usize,
    pub replayed: usize,
}

impl Summary {
    pub fn from_rows<'a>(
        rows: impl IntoIterator<Item = &'a SearchEvalRow>,
        exclude: &[String],
    ) -> Self {
        let mut s = Summary::default();
        let mut rec5 = 0.0;
        let mut rec10 = 0.0;
        for row in rows {
            let (status, score) = rescore(row, exclude);
            let results = without_sources(&row.results, exclude);
            s.n += 1;
            match status {
                CallStatus::Error => {
                    s.errors += 1;
                    *s.error_classes
                        .entry(row.error_class.unwrap_or(ErrorClass::Other))
                        .or_default() += 1;
                }
                CallStatus::Empty => s.empty += 1,
                CallStatus::Ok => {}
            }
            for (i, h) in score.hit.iter().enumerate() {
                s.hits[i] += usize::from(*h);
            }
            s.leak_hits += usize::from(score.leak_hit);
            rec5 += score.source_recall_5;
            rec10 += score.source_recall_10;
            s.all_sources_5 += usize::from(score.all_sources_5);
            s.all_sources_10 += usize::from(score.all_sources_10);
            s.latencies.push(row.latency_ms);
            if row.escalated {
                s.latencies_escalated.push(row.latency_ms);
            } else {
                s.latencies_fast.push(row.latency_ms);
            }
            for r in &results {
                s.by_source.entry(r.source.clone()).or_default().0 += 1;
            }
            if let Some(first) = results.iter().take(5).find(|r| {
                !is_leak(&r.url)
                    && contains_answer(&format!("{} {}", r.title, r.snippet), &row.gold_answer)
            }) {
                s.by_source.entry(first.source.clone()).or_default().1 += 1;
            }
            s.n_calls += row.n_calls;
            s.n_results += results.len();
            s.replayed += usize::from(row.replayed);
        }
        if s.n > 0 {
            s.source_recall_5 = rec5 / s.n as f64;
            s.source_recall_10 = rec10 / s.n as f64;
        }
        s
    }

    fn rate(&self, k: usize) -> String {
        let (lo, hi) = wilson_ci(k, self.n);
        format!(
            "{:5.1}% [{:4.1}, {:4.1}]",
            100.0 * k as f64 / self.n.max(1) as f64,
            100.0 * lo,
            100.0 * hi
        )
    }

    /// Plain-text summary table. `frames` adds the source-recall block.
    pub fn render(&self, frames: bool) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "n = {}  (replayed rows: {})", self.n, self.replayed);
        let _ = writeln!(o, "error rate        {}", self.rate(self.errors));
        let _ = writeln!(o, "empty rate        {}", self.rate(self.empty));
        let _ = writeln!(
            o,
            "error+empty       {}",
            self.rate(self.errors + self.empty)
        );
        for (i, k) in HIT_KS.iter().enumerate() {
            let _ = writeln!(o, "answer hit@{k}      {}", self.rate(self.hits[i]));
        }
        let _ = writeln!(o, "leaked hits (not counted) {}", self.leak_hits);
        if frames {
            let _ = writeln!(o, "source recall@5   {:5.1}%", 100.0 * self.source_recall_5);
            let _ = writeln!(
                o,
                "source recall@10  {:5.1}%",
                100.0 * self.source_recall_10
            );
            let _ = writeln!(o, "all-sources@5     {}", self.rate(self.all_sources_5));
            let _ = writeln!(o, "all-sources@10    {}", self.rate(self.all_sources_10));
        }
        let _ = writeln!(
            o,
            "latency ms        p50 {}  p95 {}   (fast path: p50 {} p95 {} n={}; escalated: p50 {} p95 {} n={})",
            percentile(&self.latencies, 50.0),
            percentile(&self.latencies, 95.0),
            percentile(&self.latencies_fast, 50.0),
            percentile(&self.latencies_fast, 95.0),
            self.latencies_fast.len(),
            percentile(&self.latencies_escalated, 50.0),
            percentile(&self.latencies_escalated, 95.0),
            self.latencies_escalated.len(),
        );
        let _ = writeln!(
            o,
            "calls/item {:.2}  results/item {:.2}",
            self.n_calls as f64 / self.n.max(1) as f64,
            self.n_results as f64 / self.n.max(1) as f64
        );
        if !self.error_classes.is_empty() {
            let _ = writeln!(o, "error classes     {:?}", self.error_classes);
        }
        if !self.by_source.is_empty() {
            let _ = writeln!(o, "by source (results, first-hit@5):");
            for (src, (n, h)) in &self.by_source {
                let _ = writeln!(o, "  {:<14} {:>5} {:>5}", src, n, h);
            }
        }
        o
    }
}

/// Group rows by stratum (SimpleQA topic / FRAMES primary reasoning type).
pub fn by_primary_stratum(rows: &[SearchEvalRow]) -> BTreeMap<String, Vec<&SearchEvalRow>> {
    let mut m: BTreeMap<String, Vec<&SearchEvalRow>> = BTreeMap::new();
    for r in rows {
        m.entry(r.strata.first().cloned().unwrap_or_default())
            .or_default()
            .push(r);
    }
    m
}

/// Which per-row binary outcome a paired comparison uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairedMetric {
    Hit1,
    Hit5,
    AllSources5,
    AllSources10,
    NotErrorOrEmpty,
}

impl PairedMetric {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "hit1" => Self::Hit1,
            "hit5" => Self::Hit5,
            "all_sources5" => Self::AllSources5,
            "all_sources10" => Self::AllSources10,
            "ok" => Self::NotErrorOrEmpty,
            _ => return None,
        })
    }

    fn outcome(self, row: &SearchEvalRow, exclude: &[String]) -> bool {
        let (status, score) = rescore(row, exclude);
        match self {
            Self::Hit1 => score.hit[0],
            Self::Hit5 => score.hit[2],
            Self::AllSources5 => score.all_sources_5,
            Self::AllSources10 => score.all_sources_10,
            Self::NotErrorOrEmpty => status == CallStatus::Ok,
        }
    }
}

/// `task,a_correct,b_correct` over the ids both runs share, in `a`'s order —
/// the input `examples/paired_report.rs` expects.
pub fn paired_csv(
    a: &[SearchEvalRow],
    b: &[SearchEvalRow],
    metric: PairedMetric,
    exclude: &[String],
) -> String {
    let b_by_id: BTreeMap<&str, &SearchEvalRow> = b.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut o = String::from("task,a_correct,b_correct\n");
    for ra in a {
        if let Some(rb) = b_by_id.get(ra.id.as_str()) {
            let _ = writeln!(
                o,
                "{},{},{}",
                ra.id,
                metric.outcome(ra, exclude),
                metric.outcome(rb, exclude)
            );
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(title: &str, url: &str, snippet: &str, source: &str) -> EvalResult {
        EvalResult {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
            source: source.into(),
        }
    }

    fn row(results: Vec<EvalResult>, status: CallStatus) -> SearchEvalRow {
        SearchEvalRow {
            id: "1".into(),
            dataset: "simpleqa".into(),
            provider: "keyless".into(),
            mode: "single".into(),
            queries: vec!["q".into()],
            status,
            error_class: None,
            error_text: None,
            gold_answer: "Michio Sugeno".into(),
            gold_links: vec![],
            strata: vec!["Science and technology".into()],
            results,
            latency_ms: 100,
            n_calls: 1,
            bytes: 0,
            escalated: false,
            replayed: false,
            score: None,
        }
    }

    #[test]
    fn normalize_answer_rules() {
        assert_eq!(normalize_answer("The  Beatles!"), "beatles");
        assert_eq!(normalize_answer("1,000 people"), "1000 people");
        assert_eq!(normalize_answer("Ａｂｃ"), "abc"); // NFKC full-width
        assert_eq!(normalize_answer("St. John's"), "st john s");
        assert_eq!(normalize_answer("a, b"), "b");
    }

    #[test]
    fn contains_answer_is_token_bounded() {
        assert!(contains_answer(
            "Awarded to Michio Sugeno in 2010.",
            "Michio Sugeno"
        ));
        assert!(!contains_answer("born in 2010", "10"));
        assert!(contains_answer("population: 1000", "1,000"));
        assert!(!contains_answer("anything", ""));
    }

    #[test]
    fn hit_at_k_respects_rank_and_leaks() {
        let results = vec![
            res(
                "IEEE award",
                "https://huggingface.co/datasets/x",
                "Michio Sugeno",
                "tavily",
            ),
            res("Other", "https://example.com", "nothing", "tavily"),
            res(
                "Winners",
                "https://ieee.org/a",
                "2010: Michio Sugeno",
                "tavily",
            ),
        ];
        assert!(!answer_hit_at_k("Michio Sugeno", &results, 1));
        assert!(!answer_hit_at_k("Michio Sugeno", &results, 2));
        assert!(answer_hit_at_k("Michio Sugeno", &results, 3));
        let s = score_results("Michio Sugeno", &[], &results);
        assert_eq!(s.hit, [false, true, true]);
        assert!(s.leak_hit);
    }

    #[test]
    fn leak_blocklist_matches_domains_and_paths_only() {
        assert!(is_leak(
            "https://huggingface.co/datasets/basicv8vc/SimpleQA"
        ));
        assert!(is_leak("https://www.kaggle.com/datasets/x"));
        assert!(is_leak(
            "https://github.com/openai/simple-evals/blob/main/x.py"
        ));
        assert!(is_leak("https://discuss.huggingface.co/t/x"));
        assert!(!is_leak("https://github.com/openai/other"));
        assert!(!is_leak("https://notkaggle.com/x"));
        assert!(!is_leak("https://en.wikipedia.org/wiki/Kaggle"));
    }

    #[test]
    fn wiki_url_normalization() {
        let a = normalize_wiki_url("https://en.m.wikipedia.org/wiki/Charlotte_Bront%C3%AB#Life");
        let b = normalize_wiki_url("http://EN.wikipedia.org/wiki/Charlotte Brontë/");
        assert_eq!(a, b);
        assert_eq!(a, "en.wikipedia.org/wiki/Charlotte_Brontë");
        assert_eq!(
            normalize_wiki_url("https://en.wikipedia.org/wiki/A?oldid=1"),
            "en.wikipedia.org/wiki/A"
        );
    }

    #[test]
    fn source_recall() {
        let gold = vec![
            "https://en.wikipedia.org/wiki/James_Buchanan".to_string(),
            "https://en.wikipedia.org/wiki/Harriet_Lane".to_string(),
        ];
        let results = vec![
            res(
                "x",
                "https://en.m.wikipedia.org/wiki/Harriet_Lane",
                "",
                "wikipedia",
            ),
            res("y", "https://example.com", "", "bing"),
        ];
        assert_eq!(source_matches_at_k(&gold, &results, 5), (1, 2));
        let s = score_results("", &gold, &results);
        assert!((s.source_recall_5 - 0.5).abs() < 1e-9);
        assert!(!s.all_sources_5);
    }

    #[test]
    fn exclude_source_reranks_and_can_empty_a_row() {
        let r = row(
            vec![
                res(
                    "Sugeno",
                    "https://en.wikipedia.org/wiki/Michio_Sugeno",
                    "Michio Sugeno",
                    "wikipedia",
                ),
                res("IEEE", "https://ieee.org", "Michio Sugeno won", "bing"),
            ],
            CallStatus::Ok,
        );
        let (st, sc) = rescore(&r, &[]);
        assert_eq!((st, sc.hit[0]), (CallStatus::Ok, true));
        let (st, sc) = rescore(&r, &["wikipedia".into()]);
        assert_eq!((st, sc.hit[0]), (CallStatus::Ok, true)); // bing result moved to rank 1
        let only_wiki = row(
            vec![res(
                "S",
                "https://en.wikipedia.org/wiki/S",
                "Michio Sugeno",
                "wikipedia",
            )],
            CallStatus::Ok,
        );
        assert_eq!(
            rescore(&only_wiki, &["Wikipedia".into()]).0,
            CallStatus::Empty
        );
    }

    #[test]
    fn error_classes() {
        assert_eq!(
            classify_error("Tavily API returned 429 Too Many Requests: x"),
            ErrorClass::RateLimit
        );
        assert_eq!(
            classify_error("Tavily API returned 432 : plan limit"),
            ErrorClass::CreditsQuota
        );
        assert_eq!(
            classify_error("Brave request failed: operation timed out"),
            ErrorClass::Timeout
        );
        assert_eq!(
            classify_error("Brave Search API returned 503 Service Unavailable: "),
            ErrorClass::Server5xx
        );
        assert_eq!(
            classify_error("Failed to parse Tavily response: EOF"),
            ErrorClass::Parse
        );
        assert_eq!(
            classify_error(
                "web search is unavailable: Bing failed (Bing returned results unrelated to the query (anti-scraping decoy page)); DuckDuckGo failed (DuckDuckGo returned HTTP 202 (bot challenge or block))"
            ),
            ErrorClass::BotChallengeDecoy
        );
        assert_eq!(
            classify_error("replay miss: tavily q"),
            ErrorClass::ReplayMiss
        );
        assert_eq!(classify_error("something odd"), ErrorClass::Other);
    }

    #[test]
    fn wilson_and_percentile() {
        let (lo, hi) = wilson_ci(50, 100);
        assert!((lo - 0.4038).abs() < 1e-3 && (hi - 0.5962).abs() < 1e-3);
        assert_eq!(wilson_ci(0, 0), (0.0, 0.0));
        assert_eq!(percentile(&[5, 1, 3, 2, 4], 50.0), 3);
        assert_eq!(percentile(&(1..=100).collect::<Vec<_>>(), 95.0), 95);
        assert_eq!(percentile(&[], 50.0), 0);
    }

    #[test]
    fn summary_and_paired_csv() {
        let hit = row(
            vec![res("t", "https://ieee.org", "Michio Sugeno", "tavily")],
            CallStatus::Ok,
        );
        let mut miss = row(vec![], CallStatus::Error);
        miss.id = "2".into();
        miss.error_class = Some(ErrorClass::Timeout);
        let rows = vec![hit.clone(), miss.clone()];
        let s = Summary::from_rows(&rows, &[]);
        assert_eq!((s.n, s.errors, s.empty, s.hits), (2, 1, 0, [1, 1, 1]));
        assert_eq!(s.error_classes.get(&ErrorClass::Timeout), Some(&1));
        assert_eq!(s.by_source.get("tavily"), Some(&(1, 1)));
        assert!(s.render(true).contains("all-sources@5"));

        let b = vec![row(vec![], CallStatus::Empty), miss];
        assert_eq!(
            paired_csv(&rows, &b, PairedMetric::Hit5, &[]),
            "task,a_correct,b_correct\n1,true,false\n2,false,false\n"
        );
    }
}
