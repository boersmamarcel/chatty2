//! Shared clipboard-paste elision for the chat input, used by the terminal UI
//! and available to the GPUI desktop app.
//!
//! A long paste is replaced in the input box by a short reference —
//! `[Pasted text #1 +45 lines]` — and expanded back to the full text on send.
//! Pure text helpers only (the `at_mention` split, AGE-172): rendering, key
//! handling and clipboard access stay in each frontend.

use std::collections::HashMap;

/// Opening of a paste reference; the rest of the shape is checked by
/// `parse_token`.
const TOKEN_PREFIX: &str = "[Pasted text #";

/// Pastes shorter than this many lines are inserted verbatim. A pasted URL,
/// path or one-line error is exactly the text the user wants to see and edit.
const ELIDE_MIN_LINES: usize = 6;

/// …and so are short pastes generally, however few lines they span.
const ELIDE_MIN_CHARS: usize = 800;

/// One elided paste: the text itself, kept so the token can be expanded again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PastedText {
    pub id: usize,
    pub text: String,
}

impl PastedText {
    /// Lines the paste spans, as reported in its token.
    pub fn line_count(&self) -> usize {
        self.text.lines().count().max(1)
    }

    /// The input-box reference for this paste.
    pub fn token(&self) -> String {
        token_for(self.id, self.line_count())
    }
}

/// Every paste elided so far this session, keyed by id.
///
/// Ids are monotonic and never reused, so `#1` stays `#1` for the life of the
/// session even after the user deletes the token and pastes again.
#[derive(Clone, Debug, Default)]
pub struct PasteStore {
    pastes: HashMap<usize, String>,
    next_id: usize,
}

impl PasteStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide what to insert into the input box for `text`.
    ///
    /// Returns the string to insert: the paste itself when it is short enough
    /// to read in place, otherwise a `[Pasted text #N +M lines]` token whose
    /// content is retained here.
    pub fn insert(&mut self, text: &str) -> String {
        if !should_elide(text) {
            return text.to_string();
        }

        self.next_id += 1;
        let paste = PastedText {
            id: self.next_id,
            text: text.to_string(),
        };
        let token = paste.token();
        self.pastes.insert(paste.id, paste.text);
        token
    }

    /// The full text of paste `id`, for `/paste <n>`.
    pub fn get(&self, id: usize) -> Option<&str> {
        self.pastes.get(&id).map(String::as_str)
    }

    /// Replace every live token in `input` with the text it stands for.
    ///
    /// A token whose id is unknown — because the user typed something that
    /// looks like one, or edited a real one until it no longer matches — is
    /// left exactly as it appears. What the user sees is what gets sent.
    pub fn expand(&self, input: &str) -> String {
        expand_with(input, |id| self.pastes.get(&id).map(String::as_str))
    }

    /// True when `input` contains at least one token this store can expand.
    pub fn has_live_token(&self, input: &str) -> bool {
        self.expand(input) != input
    }

    pub fn is_empty(&self) -> bool {
        self.pastes.is_empty()
    }
}

/// Whether `text` is long enough to be worth hiding behind a reference.
pub fn should_elide(text: &str) -> bool {
    text.lines().count() >= ELIDE_MIN_LINES || text.chars().count() >= ELIDE_MIN_CHARS
}

/// The reference shown in place of an elided paste.
pub fn token_for(id: usize, lines: usize) -> String {
    let unit = if lines == 1 { "line" } else { "lines" };
    format!("[Pasted text #{id} +{lines} {unit}]")
}

/// Byte ranges of the well-formed tokens in `input`, with their ids.
///
/// Well-formed is deliberately not the same as *live*: a token whose paste is
/// unknown still parses, so callers that only need the shape (the input box's
/// atomic ranges) and callers that need the content (expansion) agree on where
/// a token starts and ends.
fn find_tokens(input: &str) -> Vec<(usize, usize, usize)> {
    let mut found = Vec::new();
    let mut offset = 0;

    while let Some(start) = input[offset..].find(TOKEN_PREFIX) {
        let start = offset + start;
        let Some(end) = input[start..].find(']') else {
            break;
        };
        let end = start + end + 1;
        match parse_token(&input[start..end]) {
            Some(id) => {
                found.push((start, end, id));
                offset = end;
            }
            // Not a token after all. Resume just past this prefix rather than
            // past the bracket, so a real token that follows is still found.
            None => offset = start + TOKEN_PREFIX.len(),
        }
    }

    found
}

/// Char-column spans of the tokens in a single input-box `line`.
///
/// The terminal frontend marks these atomic so the cursor steps over a
/// reference and backspace removes all of it: chewing one character off the
/// end would leave a token that no longer resolves and is sent literally.
pub fn token_spans(line: &str) -> Vec<(usize, usize)> {
    find_tokens(line)
        .into_iter()
        .map(|(start, end, _)| (line[..start].chars().count(), line[..end].chars().count()))
        .collect()
}

/// Expand tokens in `input` using `lookup`, leaving unknown ones untouched.
///
/// The line count inside a token is display sugar — expansion keys on the id
/// alone, so a stale count still resolves rather than silently sending the
/// literal token.
fn expand_with<'a>(input: &str, lookup: impl Fn(usize) -> Option<&'a str>) -> String {
    let tokens = find_tokens(input);
    if tokens.is_empty() {
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    for (start, end, id) in tokens {
        out.push_str(&input[cursor..start]);
        match lookup(id) {
            Some(text) => out.push_str(text),
            None => out.push_str(&input[start..end]),
        }
        cursor = end;
    }
    out.push_str(&input[cursor..]);
    out
}

/// The id in `[Pasted text #7 +45 lines]`, or `None` when the shape is off.
fn parse_token(token: &str) -> Option<usize> {
    let body = token.strip_prefix(TOKEN_PREFIX)?.strip_suffix(']')?;
    let (id, tail) = body.split_once(' ')?;
    // The tail is generated, not user-authored; require its shape so a token
    // the user has edited stops matching and is sent exactly as displayed.
    let (count, unit) = tail.strip_prefix('+')?.split_once(' ')?;
    if !matches!(unit, "line" | "lines") || count.parse::<usize>().is_err() {
        return None;
    }
    id.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_paste(lines: usize) -> String {
        (1..=lines)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn short_pastes_are_inserted_verbatim() {
        let mut store = PasteStore::new();
        assert_eq!(store.insert("https://example.com"), "https://example.com");
        assert_eq!(store.insert("a\nb\nc"), "a\nb\nc");
        assert!(store.is_empty());
    }

    #[test]
    fn long_pastes_become_a_token() {
        let mut store = PasteStore::new();
        let text = long_paste(45);
        assert_eq!(store.insert(&text), "[Pasted text #1 +45 lines]");
        assert_eq!(store.get(1), Some(text.as_str()));
    }

    #[test]
    fn a_wide_single_line_paste_is_elided_too() {
        let mut store = PasteStore::new();
        let text = "x".repeat(ELIDE_MIN_CHARS);
        assert_eq!(store.insert(&text), "[Pasted text #1 +1 line]");
    }

    #[test]
    fn expand_restores_the_paste_byte_for_byte() {
        let mut store = PasteStore::new();
        let text = long_paste(45);
        let token = store.insert(&text);
        let input = format!("why does this panic?\n{token}\nthanks");

        assert_eq!(
            store.expand(&input),
            format!("why does this panic?\n{text}\nthanks")
        );
    }

    #[test]
    fn ids_are_never_reused() {
        let mut store = PasteStore::new();
        assert_eq!(store.insert(&long_paste(7)), "[Pasted text #1 +7 lines]");
        assert_eq!(store.insert(&long_paste(8)), "[Pasted text #2 +8 lines]");
        // Even though #1's token was "deleted" by the user, #2 keeps its id.
        assert_eq!(store.insert(&long_paste(9)), "[Pasted text #3 +9 lines]");
    }

    #[test]
    fn several_tokens_in_one_message_all_expand() {
        let mut store = PasteStore::new();
        let first = long_paste(6);
        let second = long_paste(7);
        let a = store.insert(&first);
        let b = store.insert(&second);

        assert_eq!(
            store.expand(&format!("{a} and {b}")),
            format!("{first} and {second}")
        );
    }

    #[test]
    fn an_edited_token_is_sent_literally() {
        let mut store = PasteStore::new();
        store.insert(&long_paste(45));

        for mangled in [
            "[Pasted text #1 +45 line s]",
            "[Pasted text #1 45 lines]",
            "[Pasted text # +45 lines]",
            "[Pasted text #1 +45 lines",
            "[Pasted text #1]",
        ] {
            assert_eq!(store.expand(mangled), mangled, "{mangled}");
        }
    }

    #[test]
    fn a_token_for_an_unknown_paste_is_left_alone() {
        let store = PasteStore::new();
        assert_eq!(
            store.expand("[Pasted text #99 +3 lines]"),
            "[Pasted text #99 +3 lines]"
        );
    }

    #[test]
    fn a_stale_line_count_still_expands() {
        let mut store = PasteStore::new();
        let text = long_paste(45);
        store.insert(&text);
        assert_eq!(store.expand("[Pasted text #1 +2 lines]"), text);
    }

    #[test]
    fn token_spans_are_char_columns() {
        let mut store = PasteStore::new();
        let token = store.insert(&long_paste(6));
        let line = format!("héllo {token} tail");
        let (start, end) = token_spans(&line)[0];

        assert_eq!(start, "héllo ".chars().count());
        assert_eq!(end, start + token.chars().count());
        assert_eq!(
            line.chars()
                .skip(start)
                .take(end - start)
                .collect::<String>(),
            token
        );
    }

    #[test]
    fn a_bare_prefix_does_not_swallow_the_token_after_it() {
        let mut store = PasteStore::new();
        let text = long_paste(6);
        let token = store.insert(&text);
        let input = format!("[Pasted text #{token}");

        assert_eq!(store.expand(&input), format!("[Pasted text #{text}"));
    }

    #[test]
    fn text_without_tokens_is_untouched() {
        let mut store = PasteStore::new();
        store.insert(&long_paste(45));
        assert_eq!(store.expand("just a message"), "just a message");
        assert!(!store.has_live_token("just a message"));
    }

    #[test]
    fn paste_content_that_looks_like_a_token_survives_the_round_trip() {
        let mut store = PasteStore::new();
        let text = format!("{}\n[Pasted text #9 +2 lines]", long_paste(6));
        let token = store.insert(&text);
        // The inner token has no entry, so it expands to itself.
        assert_eq!(store.expand(&token), text);
    }
}
