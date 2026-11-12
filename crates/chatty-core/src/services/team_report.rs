//! A team's report file, finished by the host (AGE-910).
//!
//! A team whose `team.json` names a `report` writes its result to that
//! workspace file (the deep-research writer does), where it outlives the
//! conversation (AGE-907). When the run ends, however it ends, the host
//! finishes the file with what no model in the team can be trusted to say
//! about itself:
//!
//! - **Citation check.** Every URL the report cites must be a URL one of
//!   the researchers' notes (their typed handoffs, TD-2) carries. A report
//!   that cites anything else gets a section listing it
//!   ([`citations_outside_notes`]). Whether a quote is really on its page
//!   is the checker's job: that needs the web.
//! - **Cost.** The spend the run was billed, the number of web searches
//!   and page fetches every agent in the tree made, and the wall time
//!   ([`RunTally`]), counted from the run's swarm trace, not reported by a
//!   model.
//! - **A partial report.** When no report was written (the budget ran out
//!   before the writer ran, AGE-903), the file is the leader's last answer
//!   under a heading that says so: the budget stop's final message lists
//!   every note finished so far.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use rig_core::completion::message::{Message, ToolResultContent, UserContent};
use serde_json::Value;

use crate::models::token_usage::{PriceBook, format_cost, price};
use crate::services::swarm_trace::{SwarmTrace, UsageLine};

/// The heading of the footer the host appends.
pub const COST_HEADING: &str = "## Cost of this run";

/// The heading of the citation check, when a cited URL is in no note.
pub const CITATION_CHECK_HEADING: &str = "## Citation check";

/// The heading of a report the team never wrote.
pub const PARTIAL_HEADING: &str = "# Partial report";

/// The tool that searches the web, and the one that fetches a page.
pub const SEARCH_TOOL: &str = "search_web";
pub const FETCH_TOOL: &str = "fetch";

/// What a run cost, counted by the host.
#[derive(Debug, Clone, PartialEq)]
pub struct RunTally {
    /// `search_web` calls, every agent in the tree.
    pub searches: usize,
    /// `fetch` calls, every agent in the tree.
    pub fetches: usize,
    /// Tokens billed to the root, its delegations' included.
    pub tokens: u64,
    /// Their price, when the book prices every line.
    pub usd: Option<f64>,
    pub wall: Duration,
}

impl RunTally {
    /// Count `trace`'s web calls and price what it billed with `book`.
    pub fn from_trace(trace: &SwarmTrace, book: Option<&PriceBook>, wall: Duration) -> Self {
        let mut counted = TraceTally::default();
        counted.add_trace(trace);
        counted.tally(book, wall)
    }
}

/// Web calls and billed usage summed over a run's turns: a headless run's
/// passes each hand their swarm trace on when they end, so the host adds
/// each one up before it goes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TraceTally {
    pub searches: usize,
    pub fetches: usize,
    pub billed: Vec<UsageLine>,
}

impl TraceTally {
    /// Add one turn's trace.
    pub fn add_trace(&mut self, trace: &SwarmTrace) {
        let tree = trace.tree();
        for id in tree.preorder() {
            for call in &tree.get(id).tool_calls {
                match call.name.as_str() {
                    SEARCH_TOOL => self.searches += 1,
                    FETCH_TOOL => self.fetches += 1,
                    _ => {}
                }
            }
        }
        self.billed.extend(trace.billed());
    }

    /// The tally, priced with `book` when it prices every line.
    pub fn tally(&self, book: Option<&PriceBook>, wall: Duration) -> RunTally {
        let tokens = self.billed.iter().map(UsageLine::tokens).sum();
        let usd = book.and_then(|book| {
            let lines: Vec<_> = self.billed.iter().map(UsageLine::to_token_usage).collect();
            let cost = price(&lines, book);
            (cost.unpriced_lines == 0).then_some(cost.usd)
        });
        RunTally {
            searches: self.searches,
            fetches: self.fetches,
            tokens,
            usd,
            wall,
        }
    }
}

/// The footer: spend, searches and fetches, wall time.
pub fn cost_footer(tally: &RunTally) -> String {
    let spend = match tally.usd {
        Some(usd) => format!("{} ({} tokens)", format_cost(usd), tally.tokens),
        None => format!(
            "{} tokens (not priced: a model has no prices)",
            tally.tokens
        ),
    };
    let secs = tally.wall.as_secs();
    let wall = format!(
        "{}h {:02}m {:02}s",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    );
    format!(
        "{COST_HEADING}\n\n- Spend: {spend}\n- Web searches: {}\n- Page fetches: {}\n- Wall \
         time: {wall}\n\nCounted by the host from the run's trace, not reported by a model.\n",
        tally.searches, tally.fetches
    )
}

/// Every `http(s)` URL in `text`, in order, each once: up to the first
/// space, quote or bracket, without trailing punctuation.
pub fn cited_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("http") {
        let tail = &rest[start..];
        if !(tail.starts_with("http://") || tail.starts_with("https://")) {
            rest = &tail[4..];
            continue;
        }
        let end = tail
            .find(|c: char| c.is_whitespace() || "<>\"'`()[]{}|".contains(c))
            .unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
        if !urls.iter().any(|known| known == url) {
            urls.push(url.to_string());
        }
        rest = &tail[end..];
    }
    urls
}

/// The URLs the notes' claims carry.
pub fn note_urls(notes: &[Value]) -> BTreeSet<String> {
    notes
        .iter()
        .filter_map(|note| note["claims"].as_array())
        .flatten()
        .filter_map(|claim| claim["url"].as_str())
        .map(str::to_string)
        .collect()
}

/// The URLs `report` cites that no note carries, in the report's order.
/// The cost footer and this check's own section are not the report's.
pub fn citations_outside_notes(report: &str, notes: &[Value]) -> Vec<String> {
    let known = note_urls(notes);
    cited_urls(report_body(report))
        .into_iter()
        .filter(|url| !known.contains(url))
        .collect()
}

/// `report` without anything the host appended.
fn report_body(report: &str) -> &str {
    [COST_HEADING, CITATION_CHECK_HEADING]
        .iter()
        .filter_map(|heading| report.find(heading))
        .min()
        .map_or(report, |at| &report[..at])
}

/// The notes in `messages`: the typed handoffs (`handoff`) of the
/// `invoke_agent` results that carry `claims`, in order.
pub fn notes_in_history(messages: &[Message]) -> Vec<Value> {
    let mut notes = Vec::new();
    for message in messages {
        let Message::User { content } = message else {
            continue;
        };
        for item in content.iter() {
            let UserContent::ToolResult(result) = item else {
                continue;
            };
            for part in result.content.iter() {
                if let ToolResultContent::Text(text) = part {
                    notes.extend(note_in_result(&text.text));
                }
            }
        }
    }
    notes
}

/// The note an `invoke_agent` result's JSON carries, if any.
pub fn note_in_result(result: &str) -> Option<Value> {
    let output: Value = serde_json::from_str(result).ok()?;
    let handoff = output.get("handoff")?;
    handoff["claims"].is_array().then(|| handoff.clone())
}

/// The finished report: `written` (the team's file, `None` when it wrote
/// none) or, failing that, the leader's last answer as a partial report;
/// then the citation check when a cited URL is in no note; then the cost.
pub fn finish(
    written: Option<&str>,
    last_answer: &str,
    notes: &[Value],
    tally: &RunTally,
) -> String {
    let mut report = match written.filter(|text| !text.trim().is_empty()) {
        Some(text) => text.trim_end().to_string(),
        None => format!(
            "{PARTIAL_HEADING}\n\nThe team ended before its report was written. What it had \
             finished:\n\n{}",
            last_answer.trim()
        ),
    };
    let outside = citations_outside_notes(&report, notes);
    if !outside.is_empty() {
        report.push_str(&format!(
            "\n\n{CITATION_CHECK_HEADING}\n\nThese URLs are cited but are in no researcher's \
             note, so nothing in this run backs them:\n\n"
        ));
        for url in outside {
            report.push_str(&format!("- {url}\n"));
        }
    }
    report.push_str("\n\n");
    report.push_str(&cost_footer(tally));
    report
}

/// [`finish`] the report at `workspace`/`relative`, writing the file (and
/// its directory) when the team wrote none. Returns the path.
pub fn finish_file(
    workspace: &Path,
    relative: &str,
    last_answer: &str,
    notes: &[Value],
    tally: &RunTally,
) -> std::io::Result<std::path::PathBuf> {
    let path = workspace.join(relative);
    let written = std::fs::read_to_string(&path).ok();
    let text = finish(written.as_deref(), last_answer, notes, tally);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::token_usage::{ModelRef, TokenPricing};
    use serde_json::json;

    fn note(urls: &[&str]) -> Value {
        json!({
            "sub_question": "q",
            "claims": urls.iter().map(|url| json!({
                "claim": "c", "url": url, "quote": "q", "accessed": "2026-11-12"
            })).collect::<Vec<_>>(),
            "open_questions": [],
        })
    }

    fn tally() -> RunTally {
        RunTally {
            searches: 3,
            fetches: 5,
            tokens: 1234,
            usd: Some(0.42),
            wall: Duration::from_secs(3723),
        }
    }

    #[test]
    fn cited_urls_stop_at_brackets_and_trailing_punctuation() {
        let text = "See [a](https://a.example/x?y=1). Also <http://b.example/p>, and \
                    https://a.example/x?y=1 again; httpx is not one, nor http:/c.";
        assert_eq!(
            cited_urls(text),
            ["https://a.example/x?y=1", "http://b.example/p"]
        );
    }

    /// AGE-910: a URL the writer cites from outside the notes is listed;
    /// the cost footer's own text is never read as the report's.
    #[test]
    fn a_citation_outside_the_notes_is_listed() {
        let notes = [
            note(&["https://a.example/1"]),
            note(&["https://b.example/2"]),
        ];
        let good = "Claim [1](https://a.example/1). Other (https://b.example/2).";
        assert!(citations_outside_notes(good, &notes).is_empty());
        let bad = "Claim (https://a.example/1). Made up (https://evil.example/3).";
        assert_eq!(
            citations_outside_notes(bad, &notes),
            ["https://evil.example/3"]
        );
        let finished = finish(Some(bad), "", &notes, &tally());
        let check = finished
            .split(CITATION_CHECK_HEADING)
            .nth(1)
            .expect("the check is appended");
        assert!(check.contains("- https://evil.example/3"), "{finished}");
        assert!(!check.contains("a.example"), "{finished}");
        assert_eq!(
            citations_outside_notes(&finished, &notes),
            ["https://evil.example/3"],
            "the appended sections are not the report"
        );
    }

    /// The footer ends the report: spend, searches, fetches, wall time.
    #[test]
    fn the_report_ends_with_the_cost() {
        let finished = finish(Some("# Report\n\nBody.\n"), "", &[], &tally());
        assert!(finished.starts_with("# Report\n\nBody."));
        let footer = &finished[finished.find(COST_HEADING).expect("the footer")..];
        for line in [
            "- Spend: $0.42 (1234 tokens)",
            "- Web searches: 3",
            "- Page fetches: 5",
            "- Wall time: 1h 02m 03s",
        ] {
            assert!(footer.contains(line), "{line} in {footer}");
        }
        assert!(!finished.contains(PARTIAL_HEADING));
        let unpriced = cost_footer(&RunTally {
            usd: None,
            ..tally()
        });
        assert!(
            unpriced.contains("- Spend: 1234 tokens (not priced"),
            "{unpriced}"
        );
    }

    /// No report written: the leader's last answer is the partial report.
    #[test]
    fn no_report_written_keeps_the_last_answer_as_a_partial_report() {
        let answer = "Budget of $1.00 spent. Here is what was finished:\n\n**r**\nnote";
        for written in [None, Some("  \n")] {
            let finished = finish(written, answer, &[], &tally());
            assert!(finished.starts_with(PARTIAL_HEADING), "{finished}");
            assert!(finished.contains(answer), "{finished}");
            assert!(finished.contains(COST_HEADING));
        }
    }

    #[test]
    fn finish_file_writes_the_directory_and_appends_to_a_written_report() {
        let dir = tempfile::tempdir().unwrap();
        let path = finish_file(dir.path(), "research/report.md", "answer", &[], &tally())
            .expect("written");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with(PARTIAL_HEADING)
        );
        std::fs::write(&path, "# Report\n").unwrap();
        finish_file(dir.path(), "research/report.md", "answer", &[], &tally()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# Report\n\n## Cost of this run"),
            "{text}"
        );
    }

    #[test]
    fn notes_are_the_handoffs_that_carry_claims() {
        let with = json!({ "agent": "r", "response": "x", "success": true,
                           "handoff": note(&["https://a.example/1"]) });
        let judge = json!({ "agent": "c", "response": "x", "success": true,
                            "handoff": { "verdict": "PASS" } });
        assert!(note_in_result(&with.to_string()).is_some());
        assert!(note_in_result(&judge.to_string()).is_none());
        assert!(note_in_result("not json").is_none());
    }

    /// The tally counts every agent's searches and fetches and prices the
    /// bill when the book prices every line.
    #[test]
    fn the_tally_counts_web_calls_and_prices_the_bill() {
        use crate::session::SessionEvent;
        let mut trace = SwarmTrace::new();
        for (id, name) in [
            ("1", "search_web"),
            ("2", "fetch"),
            ("3", "fetch"),
            ("4", "read_file"),
        ] {
            trace.apply(&SessionEvent::ToolCallStarted {
                id: id.to_string(),
                name: name.to_string(),
            });
        }
        let model = ModelRef {
            provider: crate::settings::models::providers_store::ProviderType::Ollama,
            model_id: "m".into(),
        };
        let usage = crate::models::token_usage::TokenUsage {
            model: Some(model.clone()),
            ..crate::models::token_usage::TokenUsage::new(1000, 500)
        };
        trace.apply(&SessionEvent::TokenUsage(usage));
        let mut book = PriceBook::default();
        book.insert(
            model,
            TokenPricing {
                input_per_million: 1.0,
                output_per_million: 2.0,
                ..TokenPricing::default()
            },
        );
        let tally = RunTally::from_trace(&trace, Some(&book), Duration::from_secs(5));
        assert_eq!((tally.searches, tally.fetches), (1, 2));
        assert_eq!(tally.tokens, 1500);
        assert!(
            (tally.usd.expect("priced") - 0.002).abs() < 1e-9,
            "{tally:?}"
        );
        assert_eq!(RunTally::from_trace(&trace, None, Duration::ZERO).usd, None);

        // Two passes add up.
        let mut passes = TraceTally::default();
        passes.add_trace(&trace);
        passes.add_trace(&trace);
        let both = passes.tally(Some(&book), Duration::ZERO);
        assert_eq!((both.searches, both.fetches, both.tokens), (2, 4, 3000));
    }
}
