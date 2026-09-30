//! The format check for what the `architecture-review` team writes (AGE-808).
//!
//! The team writes an ADR (`docs/adr/ADR-NNNN-slug.md`) or a design doc
//! (`docs/design/<component>.md`) in one fixed format: a frontmatter block
//! with a fixed set of keys, then a fixed list of headings in a fixed order.
//! [`check`] reads a document back and reports each rule as passed or
//! failed, so a run's output can be checked without reading it by eye:
//!
//! ```text
//! cargo run -p chatty-core --example check_architecture_doc -- docs/adr/ADR-0001-x.md
//! ```
//!
//! The same lists are the templates in the `arch-proposer` spec; a test
//! keeps the two in step. The check is structural only: it does not judge
//! what a section says, except that an ADR's kill criteria are not empty.

/// Which of the two formats a document declares with its `type:` key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocKind {
    Adr,
    Design,
}

/// The frontmatter keys every ADR carries, in the template's order. Keys
/// the team cannot know (`linear-issue`, `implemented-in`, …) are present
/// with an empty or null value.
pub const ADR_KEYS: &[&str] = &[
    "title",
    "type",
    "status",
    "decision-date",
    "deciders",
    "tags",
    "synthesis",
    "repo",
    "linear-issue",
    "linear-project",
    "implemented-in",
    "kill-criteria-met",
    "supersedes",
    "superseded-by",
];

/// The statuses an ADR can have; the team only ever writes `proposed`.
pub const ADR_STATUSES: &[&str] = &[
    "proposed",
    "accepted",
    "implemented",
    "reverted",
    "rejected",
    "superseded",
];

/// An ADR's headings after its `# ADR-NNNN: <title>`, in order:
/// `(level, text)`.
pub const ADR_HEADINGS: &[(usize, &str)] = &[
    (2, "Context"),
    (2, "Decision"),
    (2, "Consequences"),
    (3, "Positive"),
    (3, "Negative / trade-offs"),
    (3, "Neutral"),
    (2, "Kill criteria"),
    (2, "Alternatives considered"),
    (2, "References"),
];

/// The frontmatter keys every design doc carries. `component` may be
/// `spans` instead, for a page that sits between two components.
pub const DESIGN_KEYS: &[&str] = &[
    "title",
    "type",
    "component",
    "status",
    "tags",
    "repos",
    "adrs",
    "project",
    "sources",
    "verified",
    "created",
    "updated",
];

/// The statuses a design doc can have.
pub const DESIGN_STATUSES: &[&str] = &["current", "archived"];

/// A design doc's numbered sections after its `# <Component> — system
/// design`, in order. The first one ends with the verification date, so it
/// is matched by this prefix.
pub const DESIGN_HEADINGS: &[(usize, &str)] = &[
    (2, "1. What exists today (verified"),
    (2, "2. Components"),
    (2, "3. Data flow"),
    (2, "4. Interfaces"),
    (2, "5. State and storage"),
    (2, "6. Decisions this design rests on"),
    (2, "7. Not in this design"),
    (2, "8. Open engineering questions"),
    (2, "9. Literature"),
];

/// One rule's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleOutcome {
    pub rule: &'static str,
    pub passed: bool,
    /// What failed, or what was checked.
    pub detail: String,
}

/// Every rule's outcome for one document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocReport {
    /// `None` when the document declares neither type.
    pub kind: Option<DocKind>,
    pub rules: Vec<RuleOutcome>,
}

impl DocReport {
    pub fn passed(&self) -> bool {
        self.rules.iter().all(|rule| rule.passed)
    }

    /// The outcome of the rule of that name, if it was checked.
    pub fn rule(&self, name: &str) -> Option<&RuleOutcome> {
        self.rules.iter().find(|rule| rule.rule == name)
    }
}

/// Check `text` against the format its `type:` declares. `file_name` is
/// the document's file name, when known, for the ADR naming rule.
pub fn check(file_name: Option<&str>, text: &str) -> DocReport {
    let mut rules = Vec::new();
    let mut outcome = |rule: &'static str, passed: bool, detail: String| {
        rules.push(RuleOutcome {
            rule,
            passed,
            detail,
        })
    };

    let Some((frontmatter, body)) = split_frontmatter(text) else {
        outcome(
            "frontmatter",
            false,
            "the document does not open with a `---` frontmatter block".to_string(),
        );
        return DocReport { kind: None, rules };
    };
    outcome("frontmatter", true, "present".to_string());

    let value = |key: &str| {
        frontmatter
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| unquote(v))
    };
    let kind = match value("type").as_deref() {
        Some("adr") => DocKind::Adr,
        Some("design") => DocKind::Design,
        other => {
            outcome(
                "type",
                false,
                format!("`type:` is {other:?}, not `adr` or `design`"),
            );
            return DocReport { kind: None, rules };
        }
    };
    outcome("type", true, format!("{kind:?}"));

    let (keys, statuses, headings) = match kind {
        DocKind::Adr => (ADR_KEYS, ADR_STATUSES, ADR_HEADINGS),
        DocKind::Design => (DESIGN_KEYS, DESIGN_STATUSES, DESIGN_HEADINGS),
    };
    let present = |key: &str| frontmatter.iter().any(|(have, _)| have == key);
    let missing: Vec<&str> = keys
        .iter()
        .copied()
        .filter(|key| {
            let seam = kind == DocKind::Design && *key == "component" && present("spans");
            !present(key) && !seam
        })
        .collect();
    outcome(
        "frontmatter keys",
        missing.is_empty(),
        if missing.is_empty() {
            format!("all {} present", keys.len())
        } else {
            format!("missing: {}", missing.join(", "))
        },
    );

    let status = value("status").unwrap_or_default();
    outcome(
        "status",
        statuses.contains(&status.as_str()),
        format!("`{status}`, allowed: {}", statuses.join(" | ")),
    );

    let all_headings = headings_of(body);
    let title = all_headings.iter().find(|(level, _)| *level == 1);
    match kind {
        DocKind::Adr => {
            let number = title.and_then(|(_, text)| adr_number(text));
            outcome(
                "title heading",
                number.is_some(),
                format!(
                    "{:?}, wanted `# ADR-NNNN: <title>`",
                    title.map(|(_, text)| text.as_str())
                ),
            );
            if let Some(name) = file_name {
                let file_number = adr_file_number(name);
                outcome(
                    "file name",
                    file_number.is_some() && (number.is_none() || file_number == number),
                    format!("`{name}`, wanted `ADR-NNNN-slug.md` with the title's number"),
                );
            }
        }
        DocKind::Design => outcome(
            "title heading",
            title.is_some_and(|(_, text)| text.ends_with("— system design")),
            format!(
                "{:?}, wanted `# <Component> — system design`",
                title.map(|(_, text)| text.as_str())
            ),
        ),
    }

    let mut next = 0;
    for (level, text) in &all_headings {
        if let Some((want_level, want)) = headings.get(next)
            && level == want_level
            && (text == want || (next == 0 && kind == DocKind::Design && text.starts_with(want)))
        {
            next += 1;
        }
    }
    outcome(
        "headings in order",
        next == headings.len(),
        match headings.get(next) {
            None => format!("all {} in order", headings.len()),
            Some((level, text)) => {
                format!("`{} {text}` is missing or out of order", "#".repeat(*level))
            }
        },
    );

    if kind == DocKind::Adr {
        let criteria = section_body(body, "Kill criteria");
        let filled = criteria.as_deref().is_some_and(|s| !s.trim().is_empty());
        outcome(
            "kill criteria",
            filled,
            if filled {
                "not empty".to_string()
            } else {
                "`## Kill criteria` is missing or empty".to_string()
            },
        );
    }

    DocReport {
        kind: Some(kind),
        rules,
    }
}

/// The frontmatter's `key: value` lines and the body after it, when the
/// text opens with a `---` block that closes.
fn split_frontmatter(text: &str) -> Option<(Vec<(String, String)>, &str)> {
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let end = rest
        .match_indices("\n---")
        .find(|(i, _)| {
            let after = &rest[i + 4..];
            after.is_empty() || after.starts_with('\n') || after.starts_with("\r\n")
        })
        .map(|(i, _)| i)?;
    let keys = rest[..end]
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let top_level = !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            top_level.then(|| (key.to_string(), value.trim().to_string()))
        })
        .collect();
    Some((keys, &rest[end + 4..]))
}

fn unquote(value: &str) -> String {
    value.trim_matches(|c| c == '"' || c == '\'').to_string()
}

/// `(level, text)` of every ATX heading outside a fenced code block.
fn headings_of(body: &str) -> Vec<(usize, String)> {
    let mut in_fence = false;
    let mut headings = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !line.starts_with('#') {
            continue;
        }
        let level = line.chars().take_while(|c| *c == '#').count();
        if let Some(text) = line[level..].strip_prefix(' ') {
            headings.push((level, text.trim().to_string()));
        }
    }
    headings
}

/// What stands under `## <heading>` up to the next heading of level 1 or 2.
fn section_body(body: &str, heading: &str) -> Option<String> {
    let mut lines = body.lines();
    lines.find(|line| line.trim_end() == format!("## {heading}"))?;
    Some(
        lines
            .take_while(|line| !(line.starts_with("# ") || line.starts_with("## ")))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// `NNNN` of `ADR-NNNN: <title>`.
fn adr_number(title: &str) -> Option<u32> {
    let rest = title.strip_prefix("ADR-")?;
    let (digits, title) = rest.split_once(": ")?;
    (digits.len() == 4 && !title.trim().is_empty())
        .then(|| digits.parse().ok())
        .flatten()
}

/// `NNNN` of `ADR-NNNN-slug.md`.
fn adr_file_number(name: &str) -> Option<u32> {
    let slug = name.strip_prefix("ADR-")?.strip_suffix(".md")?;
    let (digits, slug) = slug.split_once('-')?;
    let slug_ok = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    (digits.len() == 4 && slug_ok)
        .then(|| digits.parse().ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_ADR: &str =
        include_str!("../../teams/architecture-review/fixtures/ADR-0001-good.md");
    const BAD_ADR: &str = include_str!("../../teams/architecture-review/fixtures/ADR-0002-bad.md");
    const GOOD_DESIGN: &str =
        include_str!("../../teams/architecture-review/fixtures/design-good.md");
    const BAD_DESIGN: &str = include_str!("../../teams/architecture-review/fixtures/design-bad.md");

    fn failed(report: &DocReport) -> Vec<&'static str> {
        report
            .rules
            .iter()
            .filter(|rule| !rule.passed)
            .map(|rule| rule.rule)
            .collect()
    }

    #[test]
    fn a_well_formed_adr_passes_every_rule() {
        let report = check(Some("ADR-0001-good.md"), GOOD_ADR);
        assert_eq!(report.kind, Some(DocKind::Adr));
        assert!(report.passed(), "{:#?}", report.rules);
        assert!(report.rule("kill criteria").is_some());
        assert!(report.rule("file name").is_some());
    }

    /// The bad fixture misses two keys, swaps two sections, leaves the kill
    /// criteria empty and has an unknown status; each is its own failure.
    #[test]
    fn a_malformed_adr_fails_the_rules_it_breaks() {
        let report = check(Some("ADR-0002-bad.md"), BAD_ADR);
        assert_eq!(
            failed(&report),
            [
                "frontmatter keys",
                "status",
                "headings in order",
                "kill criteria"
            ]
        );
        let keys = &report.rule("frontmatter keys").unwrap().detail;
        assert!(
            keys.contains("linear-issue") && keys.contains("superseded-by"),
            "{keys}"
        );
        assert!(
            check(Some("adr-2.md"), GOOD_ADR)
                .rule("file name")
                .is_some_and(|r| !r.passed)
        );
        assert!(
            !check(Some("ADR-0002-good.md"), GOOD_ADR).passed(),
            "number mismatch"
        );
    }

    #[test]
    fn a_well_formed_design_doc_passes_every_rule() {
        let report = check(Some("hosting.md"), GOOD_DESIGN);
        assert_eq!(report.kind, Some(DocKind::Design));
        assert!(report.passed(), "{:#?}", report.rules);
        // `spans` stands in for `component` on a seam page.
        let seam = GOOD_DESIGN.replace("component: hosting", "spans: [agent, hosting]");
        assert!(check(None, &seam).passed());
    }

    #[test]
    fn a_malformed_design_doc_fails_the_rules_it_breaks() {
        let report = check(Some("hosting.md"), BAD_DESIGN);
        assert_eq!(
            failed(&report),
            ["frontmatter keys", "title heading", "headings in order"]
        );
        assert!(
            report
                .rule("headings in order")
                .unwrap()
                .detail
                .contains("5. State and storage"),
            "{:?}",
            report.rule("headings in order")
        );
    }

    #[test]
    fn a_document_without_frontmatter_or_type_fails_early() {
        let report = check(None, "# ADR-0001: x\n## Context\n");
        assert_eq!(failed(&report), ["frontmatter"]);
        let report = check(None, "---\ntitle: x\ntype: note\n---\n# x\n");
        assert_eq!(report.kind, None);
        assert_eq!(failed(&report), ["type"]);
    }

    /// A heading inside a code block is text, not a section.
    #[test]
    fn headings_in_code_blocks_do_not_count() {
        let fenced = GOOD_ADR.replace("## References", "```\n## References\n```");
        assert_eq!(failed(&check(None, &fenced)), ["headings in order"]);
    }
}
