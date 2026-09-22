//! Single-pass HTML entity decoding, shared by the web-facing tools.
//!
//! A chained `.replace("&amp;", "&").replace("&lt;", "<")` — what
//! `fetch_tool::html_to_text` used to do — decodes twice: the first pass turns
//! `&amp;lt;` into `&lt;` and the second turns that into `<`, so a page showing
//! escaped markup comes back as markup. Scanning once, left to right, and
//! consuming each reference whole fixes that and gets numeric references
//! (`&#39;`, `&#x27;`) for free.
//!
//! The tables and the scanner came from `search_web_tool` (AGE-506) and moved
//! here when `fetch_tool` needed the same decoding (AGE-507). Both tools call
//! this one function; there is no second entity table anywhere in the tree.

/// HTML named entities for U+00A0–U+00FF in code point order: index 0 is
/// `&nbsp;` (U+00A0), index 95 is `&yuml;` (U+00FF). This covers the accented
/// Latin characters and Latin-1 punctuation real pages carry.
const LATIN1_ENTITY_NAMES: [&str; 96] = [
    "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf",
    "laquo", "not", "shy", "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro",
    "para", "middot", "cedil", "sup1", "ordm", "raquo", "frac14", "frac12", "frac34", "iquest",
    "Agrave", "Aacute", "Acirc", "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute",
    "Ecirc", "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH", "Ntilde", "Ograve", "Oacute",
    "Ocirc", "Otilde", "Ouml", "times", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute",
    "THORN", "szlig", "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil",
    "egrave", "eacute", "ecirc", "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde",
    "ograve", "oacute", "ocirc", "otilde", "ouml", "divide", "oslash", "ugrave", "uacute", "ucirc",
    "uuml", "yacute", "thorn", "yuml",
];

/// Common named entities outside the Latin-1 block.
const EXTRA_ENTITIES: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("ndash", '–'),
    ("mdash", '—'),
    ("lsquo", '‘'),
    ("rsquo", '’'),
    ("sbquo", '‚'),
    ("ldquo", '“'),
    ("rdquo", '”'),
    ("bdquo", '„'),
    ("dagger", '†'),
    ("Dagger", '‡'),
    ("bull", '•'),
    ("hellip", '…'),
    ("permil", '‰'),
    ("prime", '′'),
    ("Prime", '″'),
    ("lsaquo", '‹'),
    ("rsaquo", '›'),
    ("oline", '‾'),
    ("frasl", '⁄'),
    ("euro", '€'),
    ("trade", '™'),
    ("larr", '←'),
    ("uarr", '↑'),
    ("rarr", '→'),
    ("darr", '↓'),
    ("harr", '↔'),
    ("minus", '−'),
    ("ensp", ' '),
    ("emsp", ' '),
    ("thinsp", ' '),
];

/// Resolve one entity name (the text between `&` and `;`) to its character.
fn entity_char(name: &str) -> Option<char> {
    if let Some(digits) = name.strip_prefix('#') {
        let code = match digits.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => digits.parse::<u32>().ok()?,
        };
        return char::from_u32(code);
    }
    // A plain space keeps snippets trimmable, as the old table did.
    if name == "nbsp" {
        return Some(' ');
    }
    if let Some(index) = LATIN1_ENTITY_NAMES
        .iter()
        .position(|entity| *entity == name)
    {
        return char::from_u32(0xA0 + index as u32);
    }
    EXTRA_ENTITIES
        .iter()
        .find(|(entity, _)| *entity == name)
        .map(|(_, ch)| *ch)
}

/// Decode HTML character references — named (`&eacute;`) and numeric
/// (`&#233;`, `&#x27;`, `&#0183;`) — in a single pass, so `&amp;lt;` decodes
/// to the literal text `&lt;` rather than `<`. An unknown or malformed
/// reference is left exactly as it was written.
pub(crate) fn decode_html_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let decoded = after.find(';').and_then(|semi| {
            let name = &after[..semi];
            // Entity names are short and have no markup or whitespace in them.
            if name.is_empty() || name.len() > 32 || name.contains(['&', '<', ' ']) {
                None
            } else {
                entity_char(name).map(|ch| (ch, semi))
            }
        });
        match decoded {
            Some((ch, semi)) => {
                out.push(ch);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
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
    fn decodes_named_entities() {
        assert_eq!(
            decode_html_entities("A &amp; B &lt; C &gt; D &quot;E&quot; &apos;F&apos;"),
            "A & B < C > D \"E\" 'F'"
        );
        assert_eq!(decode_html_entities("a&nbsp;b"), "a b");
        assert_eq!(decode_html_entities("caf&eacute;"), "café");
        assert_eq!(
            decode_html_entities("M&uuml;ller &amp; S&oslash;n"),
            "Müller & Søn"
        );
    }

    #[test]
    fn decodes_numeric_entities() {
        assert_eq!(decode_html_entities("it&#39;s"), "it's");
        assert_eq!(decode_html_entities("it&#x27;s"), "it's");
        assert_eq!(decode_html_entities("&#8212;"), "—");
        assert_eq!(decode_html_entities("&#x1F600;"), "😀");
        // Zero-padded numeric references decode too.
        assert_eq!(decode_html_entities("&#0183;"), "·");
    }

    /// The bug the chained `.replace()` version had: decoding the output of a
    /// previous decode. `&amp;lt;` is a page showing the literal text `&lt;`.
    #[test]
    fn does_not_double_decode() {
        assert_eq!(decode_html_entities("&amp;lt;"), "&lt;");
        assert_eq!(decode_html_entities("&amp;amp;"), "&amp;");
    }

    #[test]
    fn passes_through_non_entities() {
        assert_eq!(decode_html_entities("plain text"), "plain text");
        assert_eq!(decode_html_entities("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(decode_html_entities("a&b;c"), "a&b;c");
        assert_eq!(decode_html_entities("&notareal;"), "&notareal;");
        // A '&' with no ';' for a long way is prose, not a truncated entity.
        assert_eq!(
            decode_html_entities("shares & the rest of this sentence;"),
            "shares & the rest of this sentence;"
        );
        assert_eq!(decode_html_entities("trailing &"), "trailing &");
    }

    #[test]
    fn decodes_query_string_ampersands() {
        assert_eq!(
            decode_html_entities("https://example.com/a?x=1&amp;y=2"),
            "https://example.com/a?x=1&y=2"
        );
    }

    /// Nothing `fetch` used to decode decodes differently now.
    ///
    /// These seven are the whole of what `fetch_tool::html_to_text` decoded
    /// before AGE-507 — its chained `.replace()` list, as it stands in git at
    /// v0.3.94 — so this is a claim about the shipped behaviour being
    /// preserved, checkable against history rather than against a table that
    /// only ever existed in an unmerged draft.
    #[test]
    fn merged_tables_cover_everything_fetch_used_to_decode() {
        for (reference, expected) in [
            ("&amp;", "&"),
            ("&lt;", "<"),
            ("&gt;", ">"),
            ("&quot;", "\""),
            ("&#39;", "'"),
            ("&apos;", "'"),
            ("&nbsp;", " "),
        ] {
            assert_eq!(
                decode_html_entities(reference),
                expected,
                "{reference} changed meaning when the tables merged"
            );
        }
    }

    /// `entity_char` resolves the Latin-1 block by *position* in
    /// `LATIN1_ENTITY_NAMES` (`0xA0 + index`), so inserting or reordering a
    /// name silently changes what every later entity decodes to, with no
    /// compile error. Pin both ends and a few interior landmarks.
    #[test]
    fn latin1_table_indices_line_up_with_code_points() {
        for (name, expected) in [
            ("nbsp", '\u{a0}'), // first
            ("cent", '¢'),
            ("copy", '©'),
            ("deg", '°'),
            ("middot", '·'),
            ("times", '×'),
            ("eacute", 'é'),
            ("divide", '÷'),
            ("uuml", 'ü'),
            ("yuml", 'ÿ'), // last, index 95
        ] {
            // `nbsp` is special-cased to a plain space before the table lookup,
            // so check its position directly.
            let resolved = if name == "nbsp" {
                LATIN1_ENTITY_NAMES
                    .iter()
                    .position(|entity| *entity == name)
                    .and_then(|index| char::from_u32(0xA0 + index as u32))
            } else {
                entity_char(name)
            };
            assert_eq!(resolved, Some(expected), "&{name}; is at the wrong index");
        }
        assert_eq!(LATIN1_ENTITY_NAMES.len(), 96);
    }
}
