//! The coding team's readiness check (AGE-1021).
//!
//! An issue the hosted coding swarm takes must say where it works, what
//! changes, which commands decide "done", and so on. Whether it does is a
//! question of structure, so it is answered here by a pure function and not
//! by a model's judgement. The vault note `dev/swarm-ready-issue` is the
//! human-facing definition; [`REQUIRED_SECTIONS`] is its machine twin, and the
//! `coding-lead` preset lists the same headings (a test keeps them equal).
//!
//! The curation team (AGE-1053) writes such issues: `curator-lead`'s final
//! answer is the `items` outcome grammar, which [`parse_items`] reads.

/// The section headings every swarm-ready issue has, in template order.
pub const REQUIRED_SECTIONS: [&str; 7] = [
    "Repo and files",
    "Change",
    "Check commands",
    "Acceptance criteria",
    "Mutation check",
    "Size",
    "Ship policy",
];

/// Whether `text` has every required section.
///
/// A section is present when some line is that heading, case-insensitively:
/// a Markdown heading (`## Check commands`) or a bold line
/// (`**Check commands:**`), with an optional trailing colon. Text that only
/// mentions the words inside a sentence does not count. On failure the error
/// names the missing sections, in template order.
pub fn issue_readiness(text: &str) -> Result<(), Vec<&'static str>> {
    let headings: Vec<String> = text.lines().filter_map(heading_of).collect();
    let missing: Vec<&'static str> = REQUIRED_SECTIONS
        .iter()
        .copied()
        .filter(|section| !headings.iter().any(|h| h.eq_ignore_ascii_case(section)))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing)
    }
}

/// The heading a line is, if it is one: its text without `#` marks, bold
/// markers or a trailing colon.
fn heading_of(line: &str) -> Option<String> {
    let line = line.trim();
    let is_heading = line.starts_with('#');
    let is_bold = line.starts_with("**") || line.starts_with("__");
    if !is_heading && !is_bold {
        return None;
    }
    let text = line
        .trim_start_matches('#')
        .trim()
        .trim_matches(|c| c == '*' || c == '_')
        .trim();
    let text = text.strip_suffix(':').unwrap_or(text).trim();
    let text = text.trim_matches(|c| c == '*' || c == '_').trim();
    // A bold sentence ("**Note:** see ...") is not a heading: it must be bold
    // all the way through.
    if is_bold && !is_heading && !line.ends_with("**") && !line.ends_with("__") {
        return None;
    }
    Some(text.to_string())
}

/// The first line of an `items` answer that carries items.
const ITEMS_PREFIX: &str = "ITEMS:";
const REJECT_PREFIX: &str = "REJECT:";
const ESCALATE_PREFIX: &str = "ESCALATE:";
/// A replan's line on the failed branch's work.
const PARTIAL_WORK_PREFIX: &str = "PARTIAL WORK:";
/// The line that closes an item's block.
const END_ITEM: &str = "--- END ITEM ---";

/// How an accepted item ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ship {
    /// `ship: auto`: merged once accepted.
    Auto,
    /// `ship: draft`: a draft PR for a human (billing, credentials,
    /// security).
    Draft,
}

/// A replan's verdict on the failed branch's work (`PARTIAL WORK:`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartialWork {
    /// The replacement items may start from the failed branch's head.
    Sound,
    /// They start from the base; the reason why.
    Unsound(String),
}

/// One item of an `items` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The id the curator suggested, unique within the answer.
    pub id: String,
    /// The item itself: its swarm-ready text, headers removed.
    pub text: String,
    /// Ids of items in the same answer that must be accepted first.
    pub depends_on: Vec<String>,
    /// What two items must not change at once: the `conflict-keys` header,
    /// else every path named under `## Repo and files`.
    pub conflict_keys: Vec<String>,
    pub ship: Ship,
}

/// A parsed `items` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedItems {
    pub items: Vec<Item>,
    /// A replan's `PARTIAL WORK:` line; `None` for a source.
    pub partial_work: Option<PartialWork>,
}

/// Why an `items` answer gave no items.
///
/// [`Rejected`](Self::Rejected) and [`Escalated`](Self::Escalated) are the
/// curation team's own verdicts, carrying its reason; every other variant
/// means the answer broke the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseItemsError {
    /// `REJECT: <why>`: the source is not codable as swarm work.
    Rejected(String),
    /// `ESCALATE: <why>`: the source needs a human decision.
    Escalated(String),
    /// The first non-empty line is none of `ITEMS:`, `REJECT:`, `ESCALATE:`.
    NoOutcome,
    /// `ITEMS:` is not followed by a whole number.
    BadCount(String),
    /// `ITEMS: <n>` and the number of blocks differ.
    CountMismatch { declared: usize, found: usize },
    /// An item's block (named by its id) has no `--- END ITEM ---` before
    /// the next item starts or the answer ends.
    Unterminated(String),
    /// A `--- END ITEM ---` closes no open block (1-based line number).
    StrayEnd(usize),
    /// An item's block (named by its id) holds no text.
    EmptyItem(String),
    /// Two blocks have the same id.
    DuplicateId(String),
    /// An item has no `ship:` header.
    MissingShip(String),
    /// An item's header line is malformed: an unknown `ship` value, an
    /// empty list, or a header given twice.
    BadHeader { id: String, line: String },
    /// `depends-on` names an id that is not in the answer (or the item
    /// itself).
    UnknownDependency { id: String, depends_on: String },
    /// A `PARTIAL WORK:` line that is neither `SOUND` nor `UNSOUND: <why>`,
    /// or a second one.
    BadPartialWork(String),
}

impl std::fmt::Display for ParseItemsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(why) => write!(f, "rejected: {why}"),
            Self::Escalated(why) => write!(f, "escalated: {why}"),
            Self::NoOutcome => write!(
                f,
                "the answer does not start with ITEMS:, REJECT: or ESCALATE:"
            ),
            Self::BadCount(count) => write!(f, "ITEMS: '{count}' is not a whole number"),
            Self::CountMismatch { declared, found } => {
                write!(f, "ITEMS: {declared}, but {found} item blocks")
            }
            Self::Unterminated(id) => write!(f, "item '{id}' has no {END_ITEM}"),
            Self::StrayEnd(line) => write!(f, "line {line}: {END_ITEM} closes no item"),
            Self::EmptyItem(id) => write!(f, "item '{id}' is empty"),
            Self::DuplicateId(id) => write!(f, "two items are named '{id}'"),
            Self::MissingShip(id) => write!(f, "item '{id}' has no ship: header"),
            Self::BadHeader { id, line } => write!(f, "item '{id}': bad header '{line}'"),
            Self::UnknownDependency { id, depends_on } => write!(
                f,
                "item '{id}' depends on '{depends_on}', which is no other item of this answer"
            ),
            Self::BadPartialWork(line) => write!(f, "bad PARTIAL WORK line '{line}'"),
        }
    }
}

impl std::error::Error for ParseItemsError {}

/// The items of a `curator-lead` answer (outcome contract `items`).
///
/// The grammar: the first non-empty line (Markdown bold or heading marks
/// aside) is `ITEMS: <n>`, `REJECT: <why>` or `ESCALATE: <why>`. After
/// `ITEMS: <n>` come exactly `n` blocks, each opened by a line
/// `--- ITEM <id> ---` and closed by a line `--- END ITEM ---`. A block
/// starts with header lines: `depends-on: <id>[, <id>]` (optional),
/// `conflict-keys: <path or crate>[, …]` (optional) and `ship: auto|draft`
/// (required); the rest, trimmed, is the item's text. A replan adds one
/// line `PARTIAL WORK: SOUND` or `PARTIAL WORK: UNSOUND: <why>` outside the
/// blocks; other lines outside the blocks are skipped.
///
/// Checked here: ids are unique, every item ships somehow, and `depends-on`
/// names only other items of the same answer. An answer is one
/// self-contained plan; re-pointing the dependants of a failed item at its
/// replacements is the queue's job, not the curator's.
pub fn parse_items(answer: &str) -> Result<ParsedItems, ParseItemsError> {
    let mut lines = answer.lines().enumerate();
    let first = lines
        .by_ref()
        .map(|(_, line)| outcome_text(line))
        .find(|line| !line.is_empty())
        .ok_or(ParseItemsError::NoOutcome)?;
    if let Some(why) = first.strip_prefix(REJECT_PREFIX) {
        return Err(ParseItemsError::Rejected(why.trim().to_string()));
    }
    if let Some(why) = first.strip_prefix(ESCALATE_PREFIX) {
        return Err(ParseItemsError::Escalated(why.trim().to_string()));
    }
    let count = first
        .strip_prefix(ITEMS_PREFIX)
        .ok_or(ParseItemsError::NoOutcome)?
        .trim();
    let declared: usize = count
        .parse()
        .map_err(|_| ParseItemsError::BadCount(count.to_string()))?;

    let mut items: Vec<Item> = Vec::new();
    let mut partial_work = None;
    // The open block: its id and its lines so far.
    let mut open: Option<(String, Vec<&str>)> = None;
    // Inside an open block, a fenced code block (``` … ```) is text, so an
    // item may quote a marker line without closing itself.
    let mut fenced = false;
    for (index, line) in lines {
        let trimmed = line.trim();
        if let Some((_, body)) = open.as_mut()
            && (fenced || trimmed.starts_with("```"))
        {
            fenced ^= trimmed.starts_with("```");
            body.push(line);
        } else if let Some(id) = item_start(trimmed) {
            if let Some((open_id, _)) = open {
                return Err(ParseItemsError::Unterminated(open_id));
            }
            open = Some((id.to_string(), Vec::new()));
        } else if trimmed == END_ITEM {
            let (id, body) = open.take().ok_or(ParseItemsError::StrayEnd(index + 1))?;
            if items.iter().any(|item| item.id == id) {
                return Err(ParseItemsError::DuplicateId(id));
            }
            items.push(item_of(id, &body)?);
        } else if let Some((_, body)) = open.as_mut() {
            body.push(line);
        } else if let Some(verdict) = outcome_text(line).strip_prefix(PARTIAL_WORK_PREFIX) {
            let bad = || ParseItemsError::BadPartialWork(trimmed.to_string());
            let verdict = verdict.trim();
            let parsed = if verdict == "SOUND" {
                PartialWork::Sound
            } else if let Some(why) = verdict.strip_prefix("UNSOUND") {
                PartialWork::Unsound(why.trim_start_matches(':').trim().to_string())
            } else {
                return Err(bad());
            };
            if partial_work.replace(parsed).is_some() {
                return Err(bad());
            }
        }
    }
    if let Some((id, _)) = open {
        return Err(ParseItemsError::Unterminated(id));
    }
    if items.len() != declared {
        return Err(ParseItemsError::CountMismatch {
            declared,
            found: items.len(),
        });
    }
    for item in &items {
        for dependency in &item.depends_on {
            if *dependency == item.id || !items.iter().any(|other| other.id == *dependency) {
                return Err(ParseItemsError::UnknownDependency {
                    id: item.id.clone(),
                    depends_on: dependency.clone(),
                });
            }
        }
    }
    Ok(ParsedItems {
        items,
        partial_work,
    })
}

/// One block's item: its leading header lines, then its text.
fn item_of(id: String, body: &[&str]) -> Result<Item, ParseItemsError> {
    let mut depends_on = None;
    let mut conflict_keys = None;
    let mut ship = None;
    let mut rest = body;
    while let Some((line, tail)) = rest.split_first() {
        let Some((key, value)) = line.trim().split_once(':') else {
            break;
        };
        let bad = || ParseItemsError::BadHeader {
            id: id.clone(),
            line: line.trim().to_string(),
        };
        let list = || -> Result<Vec<String>, ParseItemsError> {
            let list: Vec<String> = value
                .split(',')
                .map(|v| v.trim().trim_matches('`').to_string())
                .filter(|v| !v.is_empty())
                .collect();
            if list.is_empty() {
                Err(bad())
            } else {
                Ok(list)
            }
        };
        // Keys and the ship value are read without regard to case: a model
        // writes `Ship: Auto` as readily as `ship: auto`.
        let slot_was_full = match key.trim().to_ascii_lowercase().as_str() {
            "depends-on" => depends_on.replace(list()?).is_some(),
            "conflict-keys" => conflict_keys.replace(list()?).is_some(),
            "ship" => {
                let value = match value.trim().to_ascii_lowercase().as_str() {
                    "auto" => Ship::Auto,
                    "draft" => Ship::Draft,
                    _ => return Err(bad()),
                };
                ship.replace(value).is_some()
            }
            _ => break,
        };
        if slot_was_full {
            return Err(bad());
        }
        rest = tail;
    }
    let text = rest.join("\n").trim().to_string();
    if text.is_empty() {
        return Err(ParseItemsError::EmptyItem(id));
    }
    let ship = ship.ok_or_else(|| ParseItemsError::MissingShip(id.clone()))?;
    let conflict_keys = conflict_keys.unwrap_or_else(|| repo_paths(&text));
    Ok(Item {
        id,
        text,
        depends_on: depends_on.unwrap_or_default(),
        conflict_keys,
        ship,
    })
}

/// The paths an item names under `## Repo and files`: the first
/// backquoted span of each list line, in order, without repeats.
fn repo_paths(text: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if let Some(heading) = heading_of(line) {
            inside = heading.eq_ignore_ascii_case(REQUIRED_SECTIONS[0]);
            continue;
        }
        let line = line.trim();
        if !inside || !(line.starts_with('*') || line.starts_with('-')) {
            continue;
        }
        if let Some(path) = line.split('`').nth(1).filter(|p| !p.is_empty())
            && !paths.iter().any(|known| known == path)
        {
            paths.push(path.to_string());
        }
    }
    paths
}

/// An outcome line without the Markdown a model may dress it in
/// (`**ITEMS: 2**`, `## REJECT: …`), as hive's `verdict_line` reads `DONE`.
fn outcome_text(line: &str) -> &str {
    let line = line.trim();
    if line.starts_with("```") {
        // A fence around the whole answer is not the outcome line.
        return "";
    }
    line.trim_start_matches(['#', '*', '_', '>', ' '])
        .trim_end_matches(['*', '_', ' '])
}

/// The id of a `--- ITEM <id> ---` line, if `line` is one.
fn item_start(line: &str) -> Option<&str> {
    let id = line.strip_prefix("--- ITEM ")?.strip_suffix(" ---")?.trim();
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGE_1018: &str = include_str!("coding/fixtures/age-1018.md");
    const AGE_1045: &str = include_str!("coding/fixtures/age-1045.md");

    #[test]
    fn accepts_the_worked_examples() {
        assert_eq!(issue_readiness(AGE_1018), Ok(()));
        assert_eq!(issue_readiness(AGE_1045), Ok(()));
    }

    #[test]
    fn rejects_an_issue_without_check_commands() {
        let text = AGE_1018.replace("## Check commands", "## Notes");
        assert_eq!(issue_readiness(&text), Err(vec!["Check commands"]));
    }

    #[test]
    fn names_every_missing_section_in_template_order() {
        let text = "# T\n\n## Change\n1. do it\n\n## Size\nsmall\n";
        assert_eq!(
            issue_readiness(text),
            Err(vec![
                "Repo and files",
                "Check commands",
                "Acceptance criteria",
                "Mutation check",
                "Ship policy",
            ])
        );
        assert_eq!(issue_readiness("").unwrap_err().len(), 7);
    }

    #[test]
    fn matches_case_insensitively_and_accepts_bold_lines() {
        let text = "### REPO AND FILES\n**change:**\n**Check Commands**\n## acceptance CRITERIA:\n\
                    __Mutation check__\n# size\n**Ship policy:**\n";
        assert_eq!(issue_readiness(text), Ok(()));
    }

    #[test]
    fn a_mention_inside_a_sentence_is_not_a_section() {
        let text = AGE_1018.replace("## Check commands", "Run the check commands below.");
        assert_eq!(issue_readiness(&text), Err(vec!["Check commands"]));
        let text = AGE_1018.replace("## Size", "**Size** of the change is small.");
        assert_eq!(issue_readiness(&text), Err(vec!["Size"]));
    }

    /// Two items with their headers, a replan's `PARTIAL WORK:` line and
    /// notes outside the blocks.
    const TWO_ITEMS: &str = "ITEMS: 2\nPARTIAL WORK: SOUND\n\n--- ITEM div-tests ---\n\
                             ship: auto\nconflict-keys: tests/test_calc.sh\n\n## Change\n\
                             1. div 8 2 gives 4.\n--- END ITEM ---\nBetween the items.\n\
                             --- ITEM mod-tests ---\ndepends-on: div-tests\nship: draft\n\n\
                             ## Change\n1. mod 7 2 gives 1.\n\n--- END ITEM ---\nDone.\n";

    fn texts(parsed: &ParsedItems) -> Vec<&str> {
        parsed.items.iter().map(|item| item.text.as_str()).collect()
    }

    #[test]
    fn parses_items_with_their_headers() {
        let parsed = parse_items(TWO_ITEMS).unwrap();
        assert_eq!(parsed.partial_work, Some(PartialWork::Sound));
        assert_eq!(
            parsed.items,
            [
                Item {
                    id: "div-tests".to_string(),
                    text: "## Change\n1. div 8 2 gives 4.".to_string(),
                    depends_on: vec![],
                    conflict_keys: vec!["tests/test_calc.sh".to_string()],
                    ship: Ship::Auto,
                },
                Item {
                    id: "mod-tests".to_string(),
                    text: "## Change\n1. mod 7 2 gives 1.".to_string(),
                    depends_on: vec!["div-tests".to_string()],
                    conflict_keys: vec![],
                    ship: Ship::Draft,
                },
            ]
        );
        // Leading blank lines, Markdown on the outcome line and indented
        // markers are fine; zero items too.
        let none = ParsedItems {
            items: vec![],
            partial_work: None,
        };
        assert_eq!(parse_items("\n\n  ITEMS: 0\n"), Ok(none.clone()));
        assert_eq!(parse_items("**ITEMS: 0**"), Ok(none));
        let indented = "ITEMS: 1\n  --- ITEM a ---\nship: auto\nx\n  --- END ITEM ---  ";
        assert_eq!(texts(&parse_items(indented).unwrap()), ["x"]);
    }

    #[test]
    fn conflict_keys_default_to_the_repo_and_files_paths() {
        let answer = format!(
            "ITEMS: 1\n--- ITEM age-1018 ---\nship: auto\n\n{AGE_1018}\n--- END ITEM ---\n"
        );
        let parsed = parse_items(&answer).unwrap();
        let item = &parsed.items[0];
        assert_eq!(item.text, AGE_1018.trim());
        assert_eq!(issue_readiness(&item.text), Ok(()));
        assert_eq!(
            item.conflict_keys,
            ["crates/chatty-terminal/src/snapshot.rs"]
        );
        let answer = format!(
            "ITEMS: 1\n--- ITEM age-1045 ---\nship: auto\n\n{AGE_1045}\n--- END ITEM ---\n"
        );
        assert_eq!(
            parse_items(&answer).unwrap().items[0].conflict_keys,
            ["crates/chatty-terminal/tests/headless.rs"]
        );
    }

    #[test]
    fn a_replan_says_whether_the_partial_work_is_sound() {
        let answer = "ITEMS: 0\nPARTIAL WORK: UNSOUND: expects 3.5";
        assert_eq!(
            parse_items(answer).unwrap().partial_work,
            Some(PartialWork::Unsound("expects 3.5".to_string()))
        );
        for bad in [
            "ITEMS: 0\nPARTIAL WORK: maybe",
            "ITEMS: 0\nPARTIAL WORK: SOUND\nPARTIAL WORK: SOUND",
        ] {
            assert!(
                matches!(parse_items(bad), Err(ParseItemsError::BadPartialWork(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn every_item_needs_a_ship_header() {
        let answer = TWO_ITEMS.replacen("ship: auto\n", "", 1);
        assert_eq!(
            parse_items(&answer),
            Err(ParseItemsError::MissingShip("div-tests".to_string()))
        );
        let answer = TWO_ITEMS.replacen("ship: auto", "ship: yes", 1);
        assert_eq!(
            parse_items(&answer),
            Err(ParseItemsError::BadHeader {
                id: "div-tests".to_string(),
                line: "ship: yes".to_string()
            })
        );
        // A header given twice, or an empty list, is malformed too.
        let answer = TWO_ITEMS.replacen("ship: auto", "ship: auto\nship: draft", 1);
        assert!(matches!(
            parse_items(&answer),
            Err(ParseItemsError::BadHeader { .. })
        ));
        let answer = TWO_ITEMS.replacen("depends-on: div-tests", "depends-on: ,", 1);
        assert!(matches!(
            parse_items(&answer),
            Err(ParseItemsError::BadHeader { .. })
        ));
    }

    #[test]
    fn ids_are_unique_and_dependencies_stay_in_the_answer() {
        let answer = TWO_ITEMS.replacen("ITEM mod-tests", "ITEM div-tests", 1);
        assert_eq!(
            parse_items(&answer),
            Err(ParseItemsError::DuplicateId("div-tests".to_string()))
        );
        let answer = TWO_ITEMS.replacen("depends-on: div-tests", "depends-on: age-1", 1);
        assert_eq!(
            parse_items(&answer),
            Err(ParseItemsError::UnknownDependency {
                id: "mod-tests".to_string(),
                depends_on: "age-1".to_string()
            })
        );
        let answer = TWO_ITEMS.replacen("depends-on: div-tests", "depends-on: mod-tests", 1);
        assert!(matches!(
            parse_items(&answer),
            Err(ParseItemsError::UnknownDependency { .. })
        ));
    }

    #[test]
    fn reject_and_escalate_carry_their_reason() {
        assert_eq!(
            parse_items("REJECT: a blog post is not a code change\nmore"),
            Err(ParseItemsError::Rejected(
                "a blog post is not a code change".to_string()
            ))
        );
        assert_eq!(
            parse_items("## ESCALATE:   pick a CI provider first"),
            Err(ParseItemsError::Escalated(
                "pick a CI provider first".to_string()
            ))
        );
    }

    #[test]
    fn an_answer_without_an_outcome_line_is_an_error() {
        assert_eq!(parse_items(""), Err(ParseItemsError::NoOutcome));
        assert_eq!(
            parse_items("Here are the items:\nITEMS: 1"),
            Err(ParseItemsError::NoOutcome)
        );
        assert_eq!(
            parse_items("ITEMS: two"),
            Err(ParseItemsError::BadCount("two".to_string()))
        );
    }

    #[test]
    fn the_count_must_match_the_blocks() {
        let answer = TWO_ITEMS.replacen("ITEMS: 2", "ITEMS: 3", 1);
        assert_eq!(
            parse_items(&answer),
            Err(ParseItemsError::CountMismatch {
                declared: 3,
                found: 2
            })
        );
        let answer = TWO_ITEMS.replacen("ITEMS: 2", "ITEMS: 1", 1);
        assert_eq!(
            parse_items(&answer),
            Err(ParseItemsError::CountMismatch {
                declared: 1,
                found: 2
            })
        );
    }

    #[test]
    fn an_item_needs_its_end_marker() {
        // The last block never closes.
        let answer = "ITEMS: 1\n--- ITEM div-tests ---\nship: auto\n\n## Change\n";
        assert_eq!(
            parse_items(answer),
            Err(ParseItemsError::Unterminated("div-tests".to_string()))
        );
        // A block opens before the one before it closed.
        let answer = TWO_ITEMS.replacen("--- END ITEM ---", "", 1);
        assert_eq!(
            parse_items(&answer),
            Err(ParseItemsError::Unterminated("div-tests".to_string()))
        );
        // Text after a block's end is not part of the item.
        let answer = "ITEMS: 1\n--- ITEM a ---\nship: auto\nin\n--- END ITEM ---\nout\n";
        assert_eq!(texts(&parse_items(answer).unwrap()), ["in"]);
    }

    #[test]
    fn a_stray_end_or_an_empty_item_is_an_error() {
        assert_eq!(
            parse_items("ITEMS: 0\n--- END ITEM ---"),
            Err(ParseItemsError::StrayEnd(2))
        );
        assert_eq!(
            parse_items("ITEMS: 1\n--- ITEM a ---\nship: auto\n  \n--- END ITEM ---"),
            Err(ParseItemsError::EmptyItem("a".to_string()))
        );
        // `--- ITEM ---` names no id, so it is a line of text, not a block.
        assert_eq!(parse_items("ITEMS: 0\n--- ITEM ---").unwrap().items, []);
    }

    /// The dress a model puts on an answer: CRLF line ends, a code fence
    /// around the whole answer, trailing spaces, headers in another order
    /// and case, and a marker line quoted inside a fenced code block.
    #[test]
    fn parses_an_answer_as_a_model_dresses_it() {
        let crlf = TWO_ITEMS.replace('\n', "\r\n");
        assert_eq!(parse_items(&crlf), parse_items(TWO_ITEMS));

        let fenced = format!("```\n{TWO_ITEMS}```\n");
        assert_eq!(parse_items(&fenced), parse_items(TWO_ITEMS));

        let spaced = TWO_ITEMS
            .replace("--- ITEM div-tests ---", "--- ITEM div-tests ---   ")
            .replace("ship: auto", "Ship: Auto  ")
            .replace(
                "depends-on: div-tests\nship: draft",
                "ship: draft\nDepends-On: div-tests",
            );
        assert_eq!(parse_items(&spaced), parse_items(TWO_ITEMS));

        let quoted = "ITEMS: 1\n--- ITEM a ---\nship: auto\n\n## Change\n\
                      1. The answer ends with:\n```text\n--- END ITEM ---\n```\n\
                      --- END ITEM ---\n";
        let item = &parse_items(quoted).unwrap().items[0];
        assert!(
            item.text.ends_with("```text\n--- END ITEM ---\n```"),
            "{}",
            item.text
        );
        // An unclosed fence swallows the real end marker.
        let unclosed = quoted.replacen("\n```\n--- END ITEM ---\n", "\n--- END ITEM ---\n", 1);
        assert_eq!(
            parse_items(&unclosed),
            Err(ParseItemsError::Unterminated("a".to_string()))
        );
    }
}
