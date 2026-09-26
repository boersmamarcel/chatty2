//! A turn that ends by announcing its next step instead of taking it.
//!
//! Some models close a text-only message with "Let me confirm and apply the
//! fix." or "Let me check when do_query() is called:" and no tool call. In
//! an attended surface the human says "go on"; in an unattended run nobody
//! does, the text-only message is taken as the end of the task, and the run
//! finishes with the work undone. `run_headless` answers such a turn with a
//! short continue nudge ([`ANNOUNCED_STEP_NUDGE`]), at most
//! [`MAX_ANNOUNCED_STEP_NUDGES`] times per run.
//!
//! The detector is deliberately narrow: it reads only the final sentence
//! of the message's tail, so a missed announcement costs what it always
//! did, and a false one costs a single extra pass.

/// Sent as a follow-up pass when the last message announced a step and the
/// turn ended without taking it.
pub(super) const ANNOUNCED_STEP_NUDGE: &str = "Agent protocol follow-up: your last message said \
     what you would do next, but your turn ended without doing it. Continue with that step now \
     by making the tool call. If the task is actually complete, reply with your final result \
     instead.";

/// Nudges a run gets for announced-but-not-taken steps.
pub(super) const MAX_ANNOUNCED_STEP_NUDGES: usize = 2;

/// How much of the message's end the detector looks at.
const TAIL_CHARS: usize = 200;

/// Words that may lead the announcing clause: "Now let me …", "Next, I'll …".
const LEAD_INS: &[&str] = &[
    "now", "next", "first", "then", "so", "okay", "ok", "alright", "finally",
];

/// Openers that announce an action on their own.
const OPENERS: &[&str] = &[
    "let me ",
    "let's ",
    "let\u{2019}s ",
    "let us ",
    "i'll ",
    "i\u{2019}ll ",
    "i will ",
    "i'm going to ",
    "i\u{2019}m going to ",
    "i am going to ",
];

/// Openers that announce an action only after a lead-in ("Now I need to …"),
/// since without one they are as often a statement of fact.
const LED_OPENERS: &[&str] = &["i need to ", "i should ", "i can ", "i must "];

/// What may follow an opener without announcing work: a hand-back to the
/// reader ("let me know"), a stop ("I'll leave it"), or the wrap-up itself
/// ("let me summarize").
const CLOSERS: &[&str] = &[
    "know",
    "leave",
    "stop",
    "end ",
    "wait",
    "summar",
    "recap",
    "conclude",
    "wrap",
    "be ",
    "hand ",
    "keep",
    "defer",
    "let you",
    "provide the final",
    "give the final",
    "now provide",
];

/// Phrases that mark the message as a final report or answer.
const REPORT_MARKERS: &[&str] = &[
    "## summary",
    "**summary",
    "summary of changes",
    "final answer",
    "in summary",
    "to summarize",
    "to summarise",
    "task is complete",
    "task complete",
];

/// Whether `text` — the last model call of a turn, with no tool call after
/// it — ends by announcing a step it did not take.
pub(super) fn announces_untaken_step(text: &str) -> bool {
    let text = text.trim_end();
    if text.is_empty() || text.ends_with("```") {
        return false;
    }
    let lower = text.to_lowercase();
    if REPORT_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return false;
    }
    let Some(sentence) = last_sentence(text) else {
        return false;
    };
    let sentence = sentence.to_lowercase();
    if sentence.contains(" you") {
        // "I'll add tests if you want", "Let me know what you think".
        return false;
    }
    let (led, rest) = strip_lead_in(&sentence);
    let after = OPENERS
        .iter()
        .chain(if led { LED_OPENERS } else { &[] })
        .find_map(|opener| rest.strip_prefix(opener));
    match after {
        Some(after) => !CLOSERS.iter().any(|closer| after.starts_with(closer)),
        None => false,
    }
}

/// The final sentence of `text`'s last [`TAIL_CHARS`] characters, without
/// its closing punctuation or markdown emphasis; `None` when it asks a
/// question or does not start inside the tail.
fn last_sentence(text: &str) -> Option<&str> {
    let body = text.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '*' | '_'));
    if body.ends_with('?') {
        return None;
    }
    let body = body
        .trim_end_matches(['.', ':', '\u{2026}', '!'])
        .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '*' | '_'));
    let tail_start = body
        .char_indices()
        .rev()
        .nth(TAIL_CHARS - 1)
        .map_or(0, |(i, _)| i);
    let tail = &body[tail_start..];
    let mut start = None;
    let mut prev: Option<(usize, char)> = None;
    for (i, c) in tail.char_indices() {
        if let Some((_, p)) = prev
            && (c.is_whitespace() && matches!(p, '.' | '!' | '?' | ':' | '\u{2026}') || p == '\n')
        {
            start = Some(i);
        }
        prev = Some((i, c));
    }
    let start = match start {
        Some(start) => tail_start + start,
        // No boundary in the tail: the sentence is the whole message only
        // if the tail is; otherwise it started before the window.
        None if tail_start == 0 => 0,
        None => return None,
    };
    let sentence = body[start..].trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '*' | '_' | '#' | '>' | '-' | '\u{2022}')
    });
    (!sentence.is_empty()).then_some(sentence)
}

/// Strip one lead-in word ("now", "next,") from `sentence`; `true` when one
/// was there.
fn strip_lead_in(sentence: &str) -> (bool, &str) {
    for lead in LEAD_INS {
        if let Some(rest) = sentence.strip_prefix(lead)
            && let Some(rest) = rest.strip_prefix(", ").or_else(|| rest.strip_prefix(' '))
        {
            return (true, rest.trim_start());
        }
    }
    (false, sentence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcements_without_the_step_are_detected() {
        for text in [
            "I found the root cause at src/utilities/codegen.py:586 where the argument is \
             dropped when it does not appear in the expression... Let me confirm and apply the fix.",
            "**The real question**: Why is alias_map empty in the first place? Let me check when \
             do_query() is called:",
            "The helper returns early. Now I'll update the caller.",
            "Next, I will run the test suite\u{2026}",
            "I'm going to rewrite the parser loop.",
            "Now I need to look at the tokenizer.",
            "Let's open the config module.",
            "OK, let me look at the tests.",
            "The failure is in the setup.\n\n**Let me read setup.py:**",
        ] {
            assert!(announces_untaken_step(text), "{text}");
        }
    }

    #[test]
    fn closers_reports_and_questions_are_not() {
        for text in [
            "Let me know if you want X.",
            "I fixed the bug in parser.rs and added a regression test; all tests pass.\n\n\
             ## Summary\n- parser.rs: handle empty input\n- tests: new case. Let me know if \
             anything else is needed.",
            "The change is in place and the tests pass. I'll leave the rest as is.",
            "I will stop here.",
            "Here is what I changed. Let me summarize:",
            "Updated the function as follows:\n\n```rust\nfn f() {}\n```",
            "Should I also update the docs? Let me know?",
            "Let me check the logs?",
            "I'll add a test if you want.",
            "The bug was in the loop bound; I fixed it and the test passes.",
            "Now I understand the problem.",
            "I need to look at the tokenizer.",
            "",
        ] {
            assert!(!announces_untaken_step(text), "{text}");
        }
    }

    #[test]
    fn only_the_tail_counts() {
        let long = format!("Let me explain. {}", "The result is fine. ".repeat(20));
        assert!(!announces_untaken_step(&long));
        let unbroken = format!("{} let me check", "x".repeat(400));
        assert!(!announces_untaken_step(&unbroken));
    }

    #[test]
    fn the_nudge_is_a_protocol_follow_up() {
        assert!(chatty_core::services::is_protocol_follow_up_text(
            ANNOUNCED_STEP_NUDGE
        ));
    }
}
