/// Simplify a raw user message into a concise keyword query for memory search.
///
/// Memvid's combined vector+lexical search works best with short, keyword-style
/// queries rather than full natural language questions. This function strips
/// common question prefixes, filler words, and punctuation to extract the
/// content words that actually matter for recall.
///
/// Examples:
///   "what do you remember about my preferences?" → "preferences"
///   "tell me about the project architecture"     → "project architecture"
///   "I like bananas"                             → "like bananas"
pub fn simplify_memory_query(raw: &str) -> String {
    // Strip leading question patterns (case-insensitive)
    let lowered = raw.to_lowercase();
    let stripped = strip_question_prefix(&lowered);

    // Remove stop words and punctuation, keep content words
    let content_words: Vec<&str> = stripped
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty() && !is_stop_word(w))
        .take(8) // Cap at 8 content words
        .collect();

    content_words.join(" ")
}

/// Strip common question/command prefixes from a lowercased string.
fn strip_question_prefix(s: &str) -> &str {
    // Order matters: longer prefixes first to avoid partial matches
    const PREFIXES: &[&str] = &[
        "what do you remember about ",
        "what do you know about ",
        "what can you tell me about ",
        "can you tell me about ",
        "do you remember anything about ",
        "do you remember ",
        "do you know anything about ",
        "do you know ",
        "tell me about ",
        "tell me what you know about ",
        "remind me about ",
        "remind me of ",
        "recall anything about ",
        "search for ",
        "search memory for ",
        "look up ",
        "find information about ",
        "what are ",
        "what is ",
        "what was ",
        "what were ",
        "how do ",
        "how does ",
        "how is ",
    ];

    for prefix in PREFIXES {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest;
        }
    }
    s
}

fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "the"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "been"
            | "being"
            | "have"
            | "has"
            | "had"
            | "do"
            | "does"
            | "did"
            | "will"
            | "would"
            | "could"
            | "should"
            | "may"
            | "might"
            | "shall"
            | "can"
            | "i"
            | "me"
            | "my"
            | "you"
            | "your"
            | "we"
            | "our"
            | "they"
            | "their"
            | "it"
            | "its"
            | "that"
            | "this"
            | "these"
            | "those"
            | "of"
            | "in"
            | "on"
            | "at"
            | "to"
            | "for"
            | "with"
            | "from"
            | "by"
            | "about"
            | "and"
            | "or"
            | "but"
            | "not"
            | "so"
            | "if"
            | "then"
            | "also"
            | "just"
            | "very"
            | "really"
            | "please"
            | "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "how"
            | "why"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_question_prefix() {
        assert_eq!(
            simplify_memory_query("what do you remember about my preferences?"),
            "preferences"
        );
    }

    #[test]
    fn strips_tell_me_about() {
        assert_eq!(
            simplify_memory_query("tell me about the project architecture"),
            "project architecture"
        );
    }

    #[test]
    fn keeps_content_words() {
        assert_eq!(simplify_memory_query("I like bananas"), "like bananas");
    }

    #[test]
    fn caps_at_8_words() {
        let long = "rust gpui tokio serde tracing anyhow thiserror memvid rig extra words";
        let result = simplify_memory_query(long);
        assert_eq!(result.split_whitespace().count(), 8);
    }

    #[test]
    fn handles_empty() {
        assert_eq!(simplify_memory_query(""), "");
    }

    #[test]
    fn short_keyword() {
        assert_eq!(simplify_memory_query("bananas"), "bananas");
    }

    #[test]
    fn what_are_my() {
        assert_eq!(
            simplify_memory_query("what are my favorite colors?"),
            "favorite colors"
        );
    }
}
