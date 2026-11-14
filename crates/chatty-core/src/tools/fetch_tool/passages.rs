use super::{
    ERROR_MAX_LENGTH, FIND_CONTEXT_AFTER, FIND_CONTEXT_BEFORE, FIND_FALLBACK_PASSAGES,
    floor_char_boundary, html_to_text, looks_like_html,
};

/// Appended to the truncation note of a page window: the page is cached, so
/// `find` is a cheap way to the part that matters.
pub(super) const WINDOW_HINT: &str = ", or pass `find` to jump to the passage you need";

/// Take the `max_length` window of `s` that starts at `start_index`, returning
/// it with a flag saying whether anything was left over.
///
/// Truncation is not a dead end: the note says where the window sits in the
/// whole text and names the `start_index` to continue from (then `hint`), and
/// because both the end of one window and the start of the next are snapped
/// with `floor_char_boundary`, `(0, n)` followed by `(n, n)` reproduce `s`
/// with no gap and no overlap (AGE-507).
pub(super) fn window(s: &str, start_index: usize, max_length: usize, hint: &str) -> (String, bool) {
    let start = floor_char_boundary(s, start_index);
    let rest = &s[start..];
    if rest.len() <= max_length {
        return (rest.to_string(), false);
    }
    let end = floor_char_boundary(rest, max_length);
    // A window narrower than the character at its start floors to 0, which
    // would return nothing and name the same start_index again — a model
    // following the note would page forever. Emit one whole character instead.
    let end = if end == 0 {
        rest.char_indices().nth(1).map_or(rest.len(), |(i, _)| i)
    } else {
        end
    };
    let mut result = rest[..end].to_string();
    result.push_str(&format!(
        "\n\n[Content truncated: showing {start}-{} of {}. Continue with start_index={}{hint}.]",
        start + end,
        s.len(),
        start + end
    ));
    (result, true)
}

/// Turn a non-2xx response body into the text the error path returns
/// (AGE-508): run it through the same HTML-to-text extraction a success
/// response gets — an error body is frequently a full HTML error page, and
/// none of that markup is ever what the model needed — then window it to
/// `max_length`, capped at [`ERROR_MAX_LENGTH`] (the smaller of the two
/// wins, so a caller who explicitly asks for a smaller window still gets
/// it).
pub(super) fn error_body_window(
    body: &str,
    content_type: &str,
    base_url: Option<&str>,
    fragment: Option<&str>,
    start_index: usize,
    max_length: usize,
) -> (String, bool) {
    let is_html = content_type.contains("text/html") || looks_like_html(body);
    let content = if is_html {
        html_to_text(body, base_url, fragment)
    } else {
        body.to_string()
    };
    window(&content, start_index, max_length.min(ERROR_MAX_LENGTH), "")
}

/// The passages of `text` from `start_index` on that contain `query`, each
/// headed by the `start_index` it begins at, within `max_length` bytes.
///
/// The query's words match case-insensitively with any whitespace between
/// them, so a phrase split across a line or a table cell still matches.
/// Neighbouring matches share one passage. When the passages don't all fit,
/// the summary line names the `start_index` to repeat the `find` from.
/// With no exact match, the closest passages by word overlap (BM25, as
/// `search_web` ranks page text) are shown instead, marked as such.
///
/// Returns the content and whether matches were left out.
pub(super) fn find_passages(
    text: &str,
    query: &str,
    start_index: usize,
    max_length: usize,
) -> (String, bool) {
    let from = floor_char_boundary(text, start_index);
    let pattern = query
        .split_whitespace()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"\s+");
    let hits: Vec<(usize, usize)> = regex::RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .map(|re| {
            re.find_iter(&text[from..])
                .map(|m| (from + m.start(), from + m.end()))
                .collect()
        })
        .unwrap_or_default();

    if hits.is_empty() {
        return (closest_passages(text, query, from, max_length), false);
    }

    // One passage per run of nearby matches.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for &(start, end) in &hits {
        let span_start = floor_char_boundary(text, start.saturating_sub(FIND_CONTEXT_BEFORE));
        let span_end = floor_char_boundary(text, end + FIND_CONTEXT_AFTER);
        match spans.last_mut() {
            Some(last) if span_start <= last.1 => last.1 = last.1.max(span_end),
            _ => spans.push((span_start, span_end)),
        }
    }

    let mut body = String::new();
    // Where the next `find` should resume, when not every passage fit.
    let mut resume_at: Option<usize> = None;
    for &(start, end) in &spans {
        let label = format!("\n--- start_index={start} ---\n");
        let room = max_length.saturating_sub(body.len() + label.len());
        if end - start <= room {
            body.push_str(&label);
            body.push_str(&text[start..end]);
            continue;
        }
        if body.is_empty() {
            // Not even the first passage fits: show its head, resume after.
            // Always make progress, or a model following the note would
            // repeat the same call forever.
            let cut = floor_char_boundary(text, start + room.max(1));
            let cut = if cut <= start {
                text[start..]
                    .char_indices()
                    .nth(1)
                    .map_or(text.len(), |(i, _)| start + i)
            } else {
                cut
            };
            body.push_str(&label);
            body.push_str(&text[start..cut]);
            resume_at = Some(cut);
        } else {
            resume_at = Some(start);
        }
        break;
    }

    let shown = hits
        .iter()
        .filter(|(start, _)| resume_at.is_none_or(|resume| *start < resume))
        .count();
    let mut summary = format!(
        "[find \"{query}\": {} match{} from start_index={from} of {}",
        hits.len(),
        if hits.len() == 1 { "" } else { "es" },
        text.len()
    );
    match resume_at {
        Some(resume) => summary.push_str(&format!(
            ", showing the first {shown}. For the rest, repeat this find with start_index={resume}. \
             To read on from a passage, fetch without find at its start_index.]"
        )),
        None => summary.push_str(". To read on from a passage, fetch without find at its start_index.]"),
    }
    (format!("{summary}\n{body}"), resume_at.is_some())
}

/// The passages of `text` from `from` on that share the most words with
/// `query` (BM25), for a `find` with no exact match: a model's phrasing of
/// what it is looking for is rarely the document's own.
fn closest_passages(text: &str, query: &str, from: usize, max_length: usize) -> String {
    use crate::tools::passages::{PASSAGE_STRIDE, PASSAGE_WORDS, score_passages, tokenize};

    let no_match = format!(
        "[find \"{query}\": no match from start_index={from} of {}. \
         Try other words, or page through with start_index.]",
        text.len()
    );
    let query_terms = tokenize(query);
    let words: Vec<(usize, usize)> = regex::Regex::new(r"\S+")
        .map(|re| {
            re.find_iter(&text[from..])
                .map(|m| (from + m.start(), from + m.end()))
                .collect()
        })
        .unwrap_or_default();
    if query_terms.is_empty() || words.is_empty() {
        return no_match;
    }

    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut first = 0;
    loop {
        let last = (first + PASSAGE_WORDS).min(words.len()) - 1;
        spans.push((words[first].0, words[last].1));
        if last + 1 == words.len() {
            break;
        }
        first += PASSAGE_STRIDE;
    }
    let tokens: Vec<Vec<String>> = spans
        .iter()
        .map(|&(start, end)| tokenize(&text[start..end]))
        .collect();
    let scores = score_passages(&query_terms, &tokens);
    let mut ranked: Vec<usize> = (0..spans.len()).filter(|&i| scores[i] > 0.0).collect();
    ranked.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]));

    let mut chosen: Vec<(usize, usize)> = Vec::new();
    for i in ranked {
        let (start, end) = spans[i];
        if chosen.iter().all(|&(s, e)| end <= s || start >= e) {
            chosen.push((start, end));
        }
        if chosen.len() == FIND_FALLBACK_PASSAGES {
            break;
        }
    }
    if chosen.is_empty() {
        return no_match;
    }

    let mut out = format!(
        "[find \"{query}\": no exact match from start_index={from} of {}. \
         Closest passages by shared words, best first:]\n",
        text.len()
    );
    let share = max_length.saturating_sub(out.len()) / chosen.len();
    for (start, end) in chosen {
        let label = format!("\n--- start_index={start} ---\n");
        let end = (end + FIND_CONTEXT_AFTER).min(start + share.saturating_sub(label.len()));
        let end = floor_char_boundary(text, end);
        out.push_str(&label);
        out.push_str(&text[start..end]);
    }
    out
}
