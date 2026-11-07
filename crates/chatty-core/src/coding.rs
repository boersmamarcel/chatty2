//! The coding team's readiness check (AGE-1021).
//!
//! An issue the hosted coding swarm takes must say where it works, what
//! changes, which commands decide "done", and so on. Whether it does is a
//! question of structure, so it is answered here by a pure function and not
//! by a model's judgement. The vault note `dev/swarm-ready-issue` is the
//! human-facing definition; [`REQUIRED_SECTIONS`] is its machine twin, and the
//! `coding-lead` preset lists the same headings (a test keeps them equal).

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
}
