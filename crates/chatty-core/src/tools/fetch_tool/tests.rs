use super::*;

/// Extract with no URL context — the shape most of these tests want.
fn extract(html: &str) -> String {
    html_to_text(html, None, None)
}

#[test]
fn test_html_to_text_basic() {
    let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
    let text = extract(html);
    assert!(text.contains("Hello"));
    assert!(text.contains("World"));
}

#[test]
fn test_html_to_text_strips_script() {
    let html = "<p>Before</p><script>alert('xss')</script><p>After</p>";
    let text = extract(html);
    assert!(text.contains("Before"));
    assert!(text.contains("After"));
    assert!(!text.contains("alert"));
}

#[test]
fn test_html_to_text_strips_style() {
    let html = "<p>Text</p><style>body { color: red; }</style><p>More</p>";
    let text = extract(html);
    assert!(text.contains("Text"));
    assert!(text.contains("More"));
    assert!(!text.contains("color"));
}

#[test]
fn test_html_to_text_decodes_entities() {
    let html = "<p>A &amp; B &lt; C &gt; D &quot;E&quot;</p>";
    let text = extract(html);
    assert!(text.contains("A & B < C > D \"E\""));
}

// --- AGE-507 criterion 1: max_length is not required ---

#[test]
fn test_schema_does_not_require_max_length() {
    let schema = FetchTool::new(None).parameters();
    assert_eq!(
        schema["required"],
        serde_json::json!(["url"]),
        "only the URL is required; requiring max_length pushes the model to invent small values"
    );
    // …and it still has a default when omitted.
    let args: FetchToolArgs =
        serde_json::from_value(serde_json::json!({ "url": "https://example.com" })).unwrap();
    assert_eq!(args.max_length, None);
    assert_eq!(args.max_length.unwrap_or(MAX_WINDOW), 20_000);
}

// --- AGE-507 criterion 2: start_index continues a truncated fetch ---

#[test]
fn test_schema_offers_start_index() {
    let schema = FetchTool::new(None).parameters();
    assert!(schema["properties"]["start_index"].is_object());
    let args: FetchToolArgs = serde_json::from_value(
        serde_json::json!({ "url": "https://example.com", "start_index": 4000 }),
    )
    .unwrap();
    assert_eq!(args.start_index, Some(4000));
}

/// Two windows of the same size, the second continuing where the first
/// stopped, must rebuild the text exactly: no gap, no overlap.
///
/// This is the seam `call()` uses; an end-to-end `call()` test can't be
/// written here because the SSRF guard blocks loopback by design, so a
/// local test server is unreachable.
#[test]
fn test_window_chunks_join_without_gap_or_overlap() {
    let text: String = (0..500).map(|i| format!("line {i}\n")).collect();
    let size = 1000;

    let (first, truncated) = window(&text, 0, size, "");
    assert!(truncated);
    let first_body = first.split("\n\n[Content truncated").next().unwrap();
    assert_eq!(first_body.len(), size);

    let (second, _) = window(&text, size, size, "");
    let second_body = second.split("\n\n[Content truncated").next().unwrap();

    assert_eq!(format!("{first_body}{second_body}"), text[..2 * size]);
    assert!(text.starts_with(&format!("{first_body}{second_body}")));
}

/// The whole text comes back when the windows are walked to the end.
#[test]
fn test_window_walks_the_whole_text() {
    let text: String = (0..200).map(|i| format!("row {i} • ")).collect();
    let size = 97; // deliberately lands mid-multibyte-character
    let mut rebuilt = String::new();
    let mut start = 0;
    loop {
        let (chunk, truncated) = window(&text, start, size, "");
        let body = chunk.split("\n\n[Content truncated").next().unwrap();
        rebuilt.push_str(body);
        if !truncated {
            break;
        }
        start += body.len();
    }
    assert_eq!(rebuilt, text);
}

/// The truncation note names the offset to continue from, so the model
/// doesn't have to work it out (or give up, which is what it did).
#[test]
fn test_window_note_names_the_next_start_index() {
    let text = "abcdefghij";
    let (chunk, truncated) = window(text, 0, 4, "");
    assert!(truncated);
    assert_eq!(
        chunk,
        "abcd\n\n[Content truncated: showing 0-4 of 10. Continue with start_index=4.]"
    );
    let (rest, truncated) = window(text, 4, 4, "");
    assert!(truncated);
    assert!(rest.starts_with("efgh"));
}

/// A window narrower than the character it starts on must still make
/// progress: returning nothing while naming the same `start_index` would
/// send a model that follows the note round the same call forever.
#[test]
fn test_window_narrower_than_a_character_still_advances() {
    let text = "a🌍béc日本語d";
    for size in 1..=4 {
        let mut start = 0;
        let mut rebuilt = String::new();
        let mut steps = 0;
        loop {
            let (chunk, truncated) = window(text, start, size, "");
            let body = chunk.split("\n\n[Content truncated").next().unwrap();
            assert!(
                !body.is_empty(),
                "empty window at start={start} size={size} would stall paging"
            );
            rebuilt.push_str(body);
            if !truncated {
                break;
            }
            start += body.len();
            steps += 1;
            assert!(steps < 100, "paging did not terminate at size={size}");
        }
        assert_eq!(rebuilt, text, "size={size} lost or duplicated text");
    }
}

#[test]
fn test_window_past_the_end_is_empty() {
    let (chunk, truncated) = window("short", 100, 10, "");
    assert_eq!(chunk, "");
    assert!(!truncated);
}

// --- AGE-508 criterion 3: non-2xx bodies go through html_to_text and a small cap ---

#[test]
fn test_error_body_window_strips_html_error_page() {
    let body = "<html><body><nav>Site nav</nav>\
                <h1>404 Not Found</h1>\
                <p>The page you requested does not exist.</p>\
                <footer>Copyright 2026</footer></body></html>";
    let (content, truncated) = error_body_window(body, "text/html", None, None, 0, 50_000);
    assert!(!truncated);
    assert!(content.contains("404 Not Found"), "got {content:?}");
    assert!(
        content.contains("The page you requested does not exist"),
        "got {content:?}"
    );
    assert!(!content.contains("Site nav"), "chrome leaked: {content:?}");
    assert!(
        !content.contains("<html>"),
        "raw markup leaked: {content:?}"
    );
}

#[test]
fn test_error_body_window_leaves_non_html_bodies_alone() {
    let body = r#"{"error":"rate limited"}"#;
    let (content, truncated) = error_body_window(body, "application/json", None, None, 0, 50_000);
    assert!(!truncated);
    assert_eq!(content, body);
}

/// The error cap (2KB) applies even when the caller's `max_length` is
/// larger (the default is 20000) — an error page's useful content is
/// rarely more than a short message.
#[test]
fn test_error_body_window_caps_below_default_max_length() {
    let long_message: String = std::iter::repeat_n('a', ERROR_MAX_LENGTH * 5).collect();
    let body = format!("<p>{long_message}</p>");
    let (content, truncated) = error_body_window(&body, "text/html", None, None, 0, MAX_WINDOW);
    assert!(truncated);
    assert!(
        content.len() <= ERROR_MAX_LENGTH + 100, // + the truncation note
        "error body was not capped to ~{ERROR_MAX_LENGTH} bytes: {} bytes",
        content.len()
    );
}

/// A caller-supplied `max_length` smaller than the error cap still wins
/// (the smaller of the two applies).
#[test]
fn test_error_body_window_respects_smaller_caller_max_length() {
    let body = "<p>Not found, sorry about that.</p>";
    let (content, truncated) = error_body_window(body, "text/html", None, None, 0, 10);
    assert!(truncated);
    let text = content.split("\n\n[Content truncated").next().unwrap();
    assert_eq!(text.len(), 10);
}

/// The error path resolves the requested `#fragment` the same way a
/// success response does, since it reuses `html_to_text` directly.
#[test]
fn test_error_body_window_honours_fragment() {
    let body = "<p>Lead</p><h2 id=\"detail\">Detail</h2><p>More detail text.</p>";
    let (content, _) = error_body_window(
        body,
        "text/html",
        Some("https://example.com/error"),
        Some("detail"),
        0,
        50_000,
    );
    assert!(content.starts_with("Detail"), "got {content:?}");
    assert!(!content.contains("Lead"), "got {content:?}");
}

// --- AGE-507 criterion 3: navigation chrome is skipped ---

#[test]
fn test_html_to_text_skips_navigation_chrome() {
    let html = "<body><nav><a href=\"/menu\">Jump to content</a> Main menu</nav>\
                <header>Site header</header>\
                <p>Article body</p>\
                <aside>Related pages</aside>\
                <footer>Privacy policy</footer></body>";
    let text = extract(html);
    assert!(text.contains("Article body"));
    for chrome in [
        "Jump to content",
        "Main menu",
        "Site header",
        "Related pages",
        "Privacy policy",
        "/menu",
    ] {
        assert!(!text.contains(chrome), "chrome leaked into text: {text:?}");
    }
}

#[test]
fn test_html_to_text_resumes_after_nested_chrome() {
    let html = "<nav>Menu <nav>Submenu</nav> More menu</nav><p>Body</p>";
    let text = extract(html);
    assert_eq!(text, "Body");
}

// --- AGE-507 criterion 4: #fragment starts the text at that element ---

#[test]
fn test_html_to_text_starts_at_fragment() {
    let html = "<body><p>Lead paragraph</p>\
                <h2 id=\"route-description\">Route description</h2>\
                <p>The route begins…</p></body>";
    let text = html_to_text(
        html,
        Some("https://example.com/road"),
        Some("route-description"),
    );
    assert!(text.starts_with("Route description"), "got {text:?}");
    assert!(!text.contains("Lead paragraph"));
    assert!(text.contains("The route begins"));
}

#[test]
fn test_html_to_text_fragment_not_found_keeps_whole_page() {
    let html = "<p>Lead paragraph</p><h2 id=\"other\">Other</h2>";
    let text = html_to_text(html, Some("https://example.com/road"), Some("missing"));
    assert!(text.starts_with("Lead paragraph"), "got {text:?}");
}

#[test]
fn test_html_to_text_fragment_matches_whole_id_only() {
    let html = "<p>Lead</p><span data-id=\"sec\">decoy</span><span id=\"sec\">Target</span>";
    let text = html_to_text(html, None, Some("sec"));
    assert!(text.starts_with("Target"), "got {text:?}");
}

// --- AGE-507 criterion 5: links and table structure survive extraction ---

#[test]
fn test_html_to_text_emits_links_with_absolute_urls() {
    let html = "<p>See <a href=\"/wiki/Route_66\">Route 66</a> and \
                <a href=\"https://example.org/spec\">the spec</a>.</p>";
    let text = html_to_text(html, Some("https://en.wikipedia.org/wiki/Roads"), None);
    assert!(
        text.contains("Route 66 (https://en.wikipedia.org/wiki/Route_66)"),
        "got {text:?}"
    );
    assert!(
        text.contains("the spec (https://example.org/spec)"),
        "got {text:?}"
    );
}

#[test]
fn test_html_to_text_resolves_relative_links_against_the_page_path() {
    let html = "<a href=\"sibling.html\">sibling</a> <a href=\"../up.html\">up</a>";
    let text = html_to_text(html, Some("https://example.com/docs/guide/page.html"), None);
    assert!(
        text.contains("sibling (https://example.com/docs/guide/sibling.html)"),
        "got {text:?}"
    );
    assert!(
        text.contains("up (https://example.com/docs/up.html)"),
        "got {text:?}"
    );
}

#[test]
fn test_html_to_text_link_urls_are_entity_decoded_once() {
    let html = "<a href=\"/s?q=a&amp;p=2\">results</a>";
    let text = html_to_text(html, Some("https://example.com/"), None);
    assert!(
        text.contains("results (https://example.com/s?q=a&p=2)"),
        "got {text:?}"
    );
}

#[test]
fn test_html_to_text_skips_noise_links() {
    let html = "<a href=\"#cite_note_1\">[1]</a> <a href=\"javascript:void(0)\">Toggle</a> \
                <a href=\"/real\">Real</a>";
    let text = html_to_text(html, Some("https://example.com/page"), None);
    assert!(!text.contains("#cite_note_1"), "got {text:?}");
    assert!(!text.contains("javascript:"), "got {text:?}");
    assert!(
        text.contains("Real (https://example.com/real)"),
        "got {text:?}"
    );
}

/// A non-ASCII href used to panic the whole `fetch` call: the
/// `javascript:` check sliced 11 raw bytes out of the href, and the 11th
/// byte of `αααααα.html` is inside a character.
#[test]
fn test_html_to_text_handles_non_ascii_hrefs() {
    for (html, expect) in [
        ("<a href=\"αααααα.html\">Greek</a>", "Greek ("),
        ("<a href=\"日本語ペ.html\">Japanese</a>", "Japanese ("),
        ("<a href=\"ü.html\">Short</a>", "Short ("),
    ] {
        let text = html_to_text(html, Some("https://example.com/docs/"), None);
        assert!(text.contains(expect), "got {text:?}");
    }
    // …and the same href straight through `resolve_href`.
    let base = reqwest::Url::parse("https://example.com/docs/").unwrap();
    assert!(resolve_href(Some(&base), "αααααα.html").is_some());
    assert!(resolve_href(Some(&base), "日本語ペ.html").is_some());
    assert!(resolve_href(None, "αααααα.html").is_some());
    // A genuine javascript: href is still dropped.
    assert_eq!(resolve_href(Some(&base), "JavaScript:void(0)"), None);
}

#[test]
fn test_html_to_text_link_without_text_emits_no_url() {
    let html = "<p>Before<a href=\"/icon\"><img src=\"i.png\"></a>After</p>";
    let text = html_to_text(html, Some("https://example.com/"), None);
    assert!(!text.contains("/icon"), "got {text:?}");
}

#[test]
fn test_html_to_text_separates_table_cells() {
    // No whitespace at all between the cells: the separation has to come
    // from the tags, not from incidental whitespace in the source.
    let html = "<table><tr><th>Year</th><th>Winner</th></tr>\
                <tr><td>1994</td><td>Brazil</td></tr></table>";
    let text = extract(html);
    assert!(text.contains("Year | Winner"), "got {text:?}");
    assert!(text.contains("1994 | Brazil"), "got {text:?}");
}

#[test]
fn test_html_to_text_table_rows_stay_on_their_own_lines() {
    let html = "<table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>";
    let text = extract(html);
    assert_eq!(text, "a | b\nc | d");
}

// --- AGE-507 criterion 6: one entity decoder, no double decoding ---

#[test]
fn test_html_to_text_does_not_double_decode_entities() {
    // A page showing the literal text `&lt;` writes it as `&amp;lt;`.
    let html = "<p>Write &amp;lt; for a less-than sign</p>";
    let text = extract(html);
    assert_eq!(text, "Write &lt; for a less-than sign");
}

#[test]
fn test_html_to_text_decodes_numeric_entities() {
    let html = "<p>it&#39;s &#x2014; fine</p>";
    let text = extract(html);
    assert_eq!(text, "it's — fine");
}

// --- long documents: conversion, windows, find, cache ---

/// A browser shows nothing of a `display:none` block; an inline-XBRL
/// filing opens with tens of KB of such data, which filled the first
/// window before any of the document was reached.
#[test]
fn test_html_to_text_skips_hidden_elements() {
    let html = "<body><div style=\"display: none\"><div>0001973266 2025-01-01</div>\
                <ix:header>facts</ix:header></div>\
                <img style=\"display:none\" src=\"x.png\"><p>Visible text</p>\
                <div STYLE='DISPLAY:NONE'/><p>Still visible</p></body>";
    let text = extract(html);
    assert!(!text.contains("0001973266"), "got {text:?}");
    assert!(!text.contains("facts"), "got {text:?}");
    assert!(text.contains("Visible text"), "got {text:?}");
    assert!(text.contains("Still visible"), "got {text:?}");
}

#[test]
fn test_html_to_text_financial_table_cells_read_as_values() {
    let html = "<table><tr><td>Revenue</td><td></td><td>$</td><td>1,234</td><td>&#160;</td>\
                <td>(</td><td>56</td><td>)</td><td>12.5</td><td>%</td><td>—</td></tr>\
                <tr><td></td><td>Total</td><td>$</td><td>(</td><td>7</td><td>)</td></tr></table>";
    let text = extract(html);
    assert_eq!(text, "Revenue | $1,234 | (56) | 12.5% | —\nTotal | $(7)");
}

/// Filings wrap every cell's value in a `<p>`; a block element inside a
/// cell must not put each value on a line of its own.
#[test]
fn test_html_to_text_block_elements_inside_cells_stay_on_the_row() {
    let html = "<table><tr><td><p>Total revenue</p></td><td><p>$</p></td>\
                <td><div>359,747</div></td></tr>\
                <tr><td><p>Net loss</p></td><td><p>(</p></td><td><p>12</p></td>\
                <td><p>)</p></td></tr></table><p>After</p><p>the table</p>";
    let text = extract(html);
    assert_eq!(
        text,
        "Total revenue | $359,747\nNet loss | (12)\nAfter\nthe table"
    );
}

/// A page laid out as one big table cell (Paul Graham's essays, Hacker
/// News, older sites) keeps its paragraphs: only a row short enough to
/// be a table row reads its blocks as spaces.
#[test]
fn test_html_to_text_page_laid_out_in_a_table_keeps_its_paragraphs() {
    let paragraph = "word ".repeat(150);
    let html = format!(
        "<table><tr><td><img src=\"x.gif\"></td><td><font>Title<br><br>{p}<br><br>{p}\
         <p>{p}</p><p>Last paragraph.</p></font></td></tr></table>",
        p = paragraph
    );
    let text = extract(&html);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "got {text:?}");
    assert_eq!(lines[0], "Title");
    assert_eq!(lines[1], paragraph.trim());
    assert_eq!(lines[4], "Last paragraph.");
    assert!(!text.contains(SOFT_BREAK));
}

/// A hidden element whose end tag the page left out ends where a
/// browser would close it, instead of hiding the rest of the page.
#[test]
fn test_html_to_text_unclosed_hidden_element_does_not_hide_the_page() {
    let html = "<body><p style=\"display:none\">secret<p>First visible\
                <ul><li style=\"display:none\">hidden item<li>Shown item</ul>\
                <table><tr><td style=\"display:none\">x<td>Cell</table>\
                <div>After</div></body>";
    let text = extract(html);
    assert!(!text.contains("secret"), "got {text:?}");
    assert!(!text.contains("hidden item"), "got {text:?}");
    assert!(text.contains("First visible"), "got {text:?}");
    assert!(text.contains("Shown item"), "got {text:?}");
    assert!(text.contains("Cell"), "got {text:?}");
    assert!(text.contains("After"), "got {text:?}");
    // A closed one still hides all of itself, nested tags included.
    let closed = extract(
        "<p style=\"display:none\">a <b>b</b> c</p><p>Shown</p>\
         <div style=\"display:none\"><p>x<div>y</div>z</div><p>Also shown</p>",
    );
    assert_eq!(closed, "Shown\nAlso shown");
}

/// A long page made of numbered lines, for the find tests.
fn long_page() -> String {
    (0..2000)
        .map(|i| match i {
            700 => "Note 3. The total\nconsideration was $3.25 billion.\n".to_string(),
            1500 => "Later the TOTAL CONSIDERATION was adjusted.\n".to_string(),
            _ => format!("Filler line {i} about nothing in particular.\n"),
        })
        .collect()
}

/// Each passage is labelled with the offset its text starts at, so
/// `start_index` from a label reads on from exactly that passage.
#[test]
fn test_find_returns_labelled_passages_across_whitespace_and_case() {
    let text = long_page();
    let (content, more) = find_passages(&text, "total consideration", 0, MAX_WINDOW);
    assert!(!more);
    assert!(
        content.starts_with("[find \"total consideration\": 2 matches from start_index=0 of "),
        "got {content:?}"
    );
    assert!(content.contains("$3.25 billion"), "got {content:?}");
    assert!(content.contains("TOTAL CONSIDERATION was adjusted"));
    assert!(
        content.len() < 4_000,
        "passages should be local, got {}",
        content.len()
    );
    let labels: Vec<usize> = content
        .split("--- start_index=")
        .skip(1)
        .map(|rest| rest.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(labels.len(), 2);
    for (label, passage) in labels.iter().zip(content.split(" ---\n").skip(1)) {
        let passage = passage.split("\n--- start_index=").next().unwrap();
        assert!(text[*label..].starts_with(passage), "label {label} is off");
    }
}

#[test]
fn test_find_searches_from_start_index() {
    let text = long_page();
    let second = text.find("Later the TOTAL").unwrap();
    let (content, _) = find_passages(&text, "total consideration", second - 10, MAX_WINDOW);
    assert!(
        content.contains(": 1 match from start_index="),
        "got {content:?}"
    );
    assert!(!content.contains("$3.25 billion"));
}

/// Matches that don't fit the window are not lost: the summary names
/// where to repeat the find, and repeating it walks every match.
#[test]
fn test_find_pages_through_matches_that_do_not_fit() {
    let text: String = (0..300)
        .map(|i| {
            format!(
                "Row {i}: segment revenue was {i} million.\n{}\n",
                "x ".repeat(700)
            )
        })
        .collect();
    let mut start = 0;
    let mut seen = 0;
    for _ in 0..300 {
        let (content, more) = find_passages(&text, "segment revenue", start, 5_000);
        assert!(
            content.len() <= 5_000 + 300,
            "window overrun: {}",
            content.len()
        );
        seen += content.matches("segment revenue was").count();
        if !more {
            break;
        }
        let resume: usize = content
            .split("repeat this find with start_index=")
            .nth(1)
            .unwrap()
            .split('.')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(resume > start, "find did not advance");
        start = resume;
    }
    assert_eq!(seen, 300);
}

/// With no exact match, the passages sharing the most words come back,
/// marked as such — the model's phrasing is rarely the document's own.
#[test]
fn test_find_without_exact_match_ranks_passages() {
    let text = long_page();
    let (content, more) = find_passages(&text, "consideration measured at closing", 0, MAX_WINDOW);
    assert!(!more);
    assert!(content.contains("no exact match"), "got {content:?}");
    assert!(
        content.contains("consideration was $3.25 billion"),
        "got {content:?}"
    );
    assert!(content.len() <= MAX_WINDOW);

    let (content, _) = find_passages(&text, "zebra quokka", 0, MAX_WINDOW);
    assert!(content.contains("no match"), "got {content:?}");
    assert!(content.contains("page through with start_index"));
}

#[test]
fn test_find_handles_regex_metacharacters_and_tiny_windows() {
    let text = "Price (USD): $3.25 [approx.]\n".repeat(3);
    let (content, _) = find_passages(&text, "$3.25 [approx.]", 0, MAX_WINDOW);
    assert!(content.contains(": 3 matches"), "got {content:?}");
    let (content, more) = find_passages(&text, "(USD)", 0, 1);
    assert!(more);
    assert!(content.contains("start_index=0 ---\nP"), "got {content:?}");
}

/// The truncation note of a page says where the window sits and how to
/// get the rest — the model gave up on pages whose note it never saw.
#[test]
fn test_window_note_offers_find() {
    let text = "a".repeat(100);
    let (content, truncated) = window(&text, 10, 20, WINDOW_HINT);
    assert!(truncated);
    assert!(
        content.ends_with(
            "[Content truncated: showing 10-30 of 100. Continue with start_index=30, \
             or pass `find` to jump to the passage you need.]"
        ),
        "got {content:?}"
    );
}

fn page(text: &str) -> Arc<CachedPage> {
    Arc::new(CachedPage {
        status: 200,
        content_type: "text/html".to_string(),
        text: text.to_string(),
        note: None,
    })
}

#[test]
fn test_page_cache_evicts_least_recently_used() {
    let cache = PageCache::default();
    for i in 0..MAX_CACHED_PAGES {
        cache.insert(format!("https://example.com/{i}"), page("x"));
    }
    // Touch the oldest so the second-oldest goes first.
    assert!(cache.get("https://example.com/0").is_some());
    cache.insert("https://example.com/new".to_string(), page("y"));
    assert!(cache.get("https://example.com/0").is_some());
    assert!(cache.get("https://example.com/1").is_none());
    assert!(cache.get("https://example.com/new").is_some());
    assert_eq!(cache.lock().len(), MAX_CACHED_PAGES);
}

#[test]
fn test_page_cache_is_bounded_by_bytes() {
    let cache = PageCache::default();
    let big = "x".repeat(MAX_CACHED_BYTES / 2 + 1);
    cache.insert("https://example.com/a".to_string(), page(&big));
    cache.insert("https://example.com/b".to_string(), page(&big));
    assert!(cache.get("https://example.com/a").is_none());
    assert!(cache.get("https://example.com/b").is_some());
    let huge = "x".repeat(MAX_CACHED_BYTES + 1);
    cache.insert("https://example.com/huge".to_string(), page(&huge));
    assert!(cache.get("https://example.com/huge").is_none());
}

/// Paging and find over a cached page never touch the network: the URL
/// here does not resolve, so any download would fail the call.
#[tokio::test]
async fn test_call_pages_and_finds_in_the_session_cache() {
    let tool = FetchTool::new(None);
    let url = "https://doc.example.invalid/10-q.htm";
    tool.pages.insert(format!("{url}\n"), page(&long_page()));
    let try_call = |start_index: Option<usize>,
                    max_length: Option<usize>,
                    find: Option<&str>,
                    user_agent: Option<&str>| {
        let tool = tool.clone();
        let find = find.map(str::to_string);
        let user_agent = user_agent.map(str::to_string);
        async move {
            tool.call(
                &mut ToolContext::new(),
                FetchToolArgs {
                    url: url.to_string(),
                    max_length,
                    start_index,
                    find,
                    user_agent,
                },
            )
            .await
        }
    };
    let call = |start_index: Option<usize>, max_length: Option<usize>, find: Option<&str>| {
        let call = try_call(start_index, max_length, find, None);
        async move { call.await.expect("served from the cache") }
    };

    // A plain fetch downloads again (here: fails, the host doesn't
    // resolve), so a changing page is never served stale; so does a
    // different User-Agent, even when paging.
    assert!(try_call(None, None, None, None).await.is_err());
    assert!(
        try_call(Some(0), None, None, Some("Research bot admin@example.com"))
            .await
            .is_err()
    );

    let first = call(Some(0), None, None).await;
    assert!(first.truncated);
    assert_eq!(first.total_length, Some(long_page().len()));
    assert!(
        first
            .content
            .contains("Continue with start_index=20000, or pass `find`")
    );

    // max_length can't take a window past what the context shaper keeps.
    let clamped = call(Some(0), Some(200_000), None).await;
    assert_eq!(clamped.content, first.content);

    let found = call(None, None, Some("total consideration")).await;
    assert!(
        found.content.contains("$3.25 billion"),
        "got {:?}",
        found.content
    );
    assert!(!found.truncated);

    let next = call(Some(20_000), None, None).await;
    assert!(long_page()[20_000..].starts_with(next.content.split("\n\n[Content").next().unwrap()));
}

// --- helpers ---

#[test]
fn test_attr_value_reads_quoted_and_unquoted_values() {
    assert_eq!(
        attr_value("a href=\"/x\" id='y'", "href").as_deref(),
        Some("/x")
    );
    assert_eq!(
        attr_value("a href=\"/x\" id='y'", "id").as_deref(),
        Some("y")
    );
    assert_eq!(
        attr_value("a href=/x class=b", "href").as_deref(),
        Some("/x")
    );
    assert_eq!(attr_value("a HREF=\"/x\"", "href").as_deref(), Some("/x"));
    assert_eq!(attr_value("span data-id=\"n\"", "id"), None);
    assert_eq!(
        attr_value("a download href=\"/x\"", "href").as_deref(),
        Some("/x")
    );
    assert_eq!(attr_value("br/", "href"), None);
    assert_eq!(
        attr_value("a title=\"héllo wörld\" href=\"/x\"", "href").as_deref(),
        Some("/x")
    );
}

#[test]
fn test_looks_like_html() {
    assert!(looks_like_html("<!DOCTYPE html><html>"));
    assert!(looks_like_html("  <!doctype html>"));
    assert!(looks_like_html("<html><head>"));
    assert!(!looks_like_html("{\"key\": \"value\"}"));
    assert!(!looks_like_html("plain text"));
}

#[test]
fn test_truncate_at_char_boundary() {
    let (truncated, flagged) = window("Hello, World!", 0, 5, "");
    assert!(truncated.starts_with("Hello"));
    assert!(truncated.contains("[Content truncated"));
    assert!(flagged);
}

#[test]
fn test_truncate_no_truncation_needed() {
    let (result, truncated) = window("short", 0, 100, "");
    assert_eq!(result, "short");
    assert!(!truncated);
}

#[test]
fn test_truncate_multibyte_chars() {
    // Ensure we don't split in the middle of a multi-byte character
    let (truncated, _) = window("Hello 🌍 World", 0, 7, "");
    // Should not panic and should produce valid UTF-8
    assert!(truncated.starts_with("Hello "));
    assert!(truncated.contains("[Content truncated"));
}

/// A window that starts mid-character starts at the character instead —
/// the same rounding the previous window's end used, which is what makes
/// the two meet exactly.
#[test]
fn test_window_start_snaps_to_char_boundary() {
    let text = "a🌍b";
    let (first, _) = window(text, 0, 3, "");
    let first_body = first.split("\n\n[Content truncated").next().unwrap();
    assert_eq!(first_body, "a");
    let (second, _) = window(text, 3, 10, "");
    assert_eq!(format!("{first_body}{second}"), text);
}
