/// Elements whose text is site furniture rather than page content. Skipped
/// whole, the way `<script>`/`<style>` are: on a Wikipedia article they are the
/// "Jump to content / Main menu / …" preamble that used to fill a small
/// `max_length` window before the article body was ever reached (AGE-507).
const CHROME_TAGS: [&str; 4] = ["nav", "header", "footer", "aside"];

/// Elements that never have a closing tag, so can't open a skipped block.
const VOID_TAGS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Placeholder between table cells while extracting; `tidy_table_rows`
/// turns it into ` | ` once the row's cells are known.
const CELL_BREAK: char = '\u{1F}';

/// Whether tag `element` (a closing tag when `closing`) ends an open
/// `hidden` element whose end tag may be omitted, per HTML's implied end
/// tags: a `<p>` ends at the next block, an `<li>` at the next item or the
/// end of its list, a cell at the next cell or row, and so on.
fn implicitly_closes(hidden: &str, element: &str, closing: bool) -> bool {
    const P_ENDERS: [&str; 22] = [
        "p",
        "div",
        "table",
        "ul",
        "ol",
        "dl",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "section",
        "article",
        "blockquote",
        "pre",
        "hr",
        "main",
        "header",
        "footer",
        "nav",
        "form",
    ];
    const P_CONTAINERS: [&str; 10] = [
        "div",
        "td",
        "th",
        "li",
        "dd",
        "section",
        "article",
        "blockquote",
        "main",
        "body",
    ];
    match hidden {
        "p" if closing => P_CONTAINERS.contains(&element),
        "p" => P_ENDERS.contains(&element),
        "li" if closing => matches!(element, "ul" | "ol" | "menu" | "body"),
        "li" => element == "li",
        "td" | "th" if closing => matches!(
            element,
            "tr" | "table" | "tbody" | "thead" | "tfoot" | "body"
        ),
        "td" | "th" => matches!(element, "td" | "th" | "tr"),
        "tr" if closing => matches!(element, "table" | "tbody" | "thead" | "tfoot" | "body"),
        "tr" => element == "tr",
        "dt" | "dd" if closing => matches!(element, "dl" | "body"),
        "dt" | "dd" => matches!(element, "dt" | "dd"),
        "option" if closing => matches!(element, "select" | "datalist" | "optgroup" | "body"),
        "option" => matches!(element, "option" | "optgroup"),
        _ => false,
    }
}

/// Placeholder for a block element inside a table cell; see
/// `resolve_cell_breaks`.
pub(super) const SOFT_BREAK: char = '\u{1E}';

/// A line of extracted text longer than this is not a table row but a page
/// laid out as a table (a Paul Graham essay, Hacker News, older sites): its
/// block elements go back to being line breaks.
const MAX_ROW_BYTES: usize = 2_000;

/// Settle the [`SOFT_BREAK`]s block elements inside table cells left behind.
/// On a table row they become spaces, so a filing that wraps each cell's
/// value in a `<p>` still reads as one row; on a line longer than
/// [`MAX_ROW_BYTES`] they become newlines, or a page laid out as one big
/// cell would come out as a single line of tens of KB. Neighbouring
/// whitespace folds into the break either way.
fn resolve_cell_breaks(text: &str) -> String {
    if !text.contains(SOFT_BREAK) {
        return text.to_string();
    }
    text.split('\n')
        .map(|line| {
            if !line.contains(SOFT_BREAK) {
                return line.to_string();
            }
            let brk = if line.len() > MAX_ROW_BYTES {
                '\n'
            } else {
                ' '
            };
            let mut out = String::with_capacity(line.len());
            let mut pending_break = false;
            for ch in line.chars() {
                if ch == SOFT_BREAK {
                    pending_break = true;
                    continue;
                }
                if pending_break && ch == ' ' {
                    continue;
                }
                if pending_break {
                    let kept = out.trim_end_matches(' ').len();
                    out.truncate(kept);
                    if !out.is_empty() && !out.ends_with(CELL_BREAK) {
                        out.push(brk);
                    }
                    pending_break = false;
                }
                out.push(ch);
            }
            out
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether a raw opening tag hides its element (`style="display: none"`):
/// a browser shows none of that text, and documents use such blocks for
/// machine-readable data — an inline-XBRL filing opens with tens of KB of
/// it — that would otherwise fill the first window.
fn hides_element(tag: &str) -> bool {
    attr_value(tag, "style").is_some_and(|style| {
        style
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase()
            .contains("display:none")
    })
}

/// Rebuild the table rows of extracted text: drop empty cells (layout
/// spacers), glue a cell holding only a currency sign or bracket onto its
/// neighbour (`$` and `(` open the number after them, `)` and `%` close the
/// one before), and separate what is left with ` | `. Any other cell stays
/// its own, a lone `—` included: tables use it for a nil value. Financial tables put each of those
/// in a cell of its own, which read as `$ | | 1,234 | ) | |` before.
fn tidy_table_rows(text: &str) -> String {
    if !text.contains(CELL_BREAK) {
        return text.to_string();
    }
    let opens_next = |cell: &str| cell.chars().all(|ch| "$(€£¥".contains(ch));
    let closes_previous = |cell: &str| cell.chars().all(|ch| ")%".contains(ch));
    text.split('\n')
        .map(|line| {
            if !line.contains(CELL_BREAK) {
                return line.to_string();
            }
            let mut cells: Vec<String> = Vec::new();
            let mut pending = String::new();
            for cell in line
                .split(CELL_BREAK)
                .map(str::trim)
                .filter(|c| !c.is_empty())
            {
                if opens_next(cell) {
                    pending.push_str(cell);
                } else if closes_previous(cell) && pending.is_empty() && !cells.is_empty() {
                    if let Some(last) = cells.last_mut() {
                        last.push_str(cell);
                    }
                } else {
                    cells.push(format!("{}{cell}", std::mem::take(&mut pending)));
                }
            }
            if !pending.is_empty() {
                cells.push(pending);
            }
            cells.join(" | ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The lowercase tag name of a raw tag body (`a href="/x"` → `a`,
/// `/div ` → `/div`), keeping the leading slash of a closing tag.
fn tag_name(tag: &str) -> String {
    tag.chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '/')
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// The value of attribute `name` in a raw tag body, quoted or not.
///
/// Attribute names are matched whole and case-insensitively, so looking for
/// `id` doesn't match `data-id` and looking for `href` doesn't match `xhref`.
/// Entity references in the value are left alone — the whole extracted text is
/// decoded once at the end, and decoding here too would decode twice.
pub(super) fn attr_value(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    // Every index below lands on a char boundary: the scan only ever stops on
    // an ASCII delimiter, and a UTF-8 continuation byte is never one.
    let mut i = 0;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1; // the tag name itself
    }
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        let key = &tag[key_start..i];
        if key.is_empty() {
            i += 1;
            continue;
        }
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            continue; // valueless attribute, e.g. `disabled`
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let value = if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
            let quote = bytes[j];
            j += 1;
            let value_start = j;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            let value = &tag[value_start..j];
            j = (j + 1).min(bytes.len());
            value
        } else {
            let value_start = j;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            &tag[value_start..j]
        };
        i = j;
        if key.eq_ignore_ascii_case(name) {
            return Some(value.to_string());
        }
    }
    None
}

/// Turn an `href` into an absolute URL to print next to its link text.
///
/// Returns `None` for links that would only add noise: empty hrefs, in-page
/// `#fragment` links (the model can build those from the page URL itself), and
/// `javascript:` handlers.
pub(super) fn resolve_href(base: Option<&reqwest::Url>, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    // `get` rather than `href[..11]`: an href whose 11th byte falls inside a
    // multi-byte character (`αααααα.html`) would panic the whole fetch call.
    if href
        .get(..11)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("javascript:"))
    {
        return None;
    }
    match base {
        Some(base) => base.join(href).ok().map(String::from),
        None => Some(href.to_string()),
    }
}

/// Convert HTML to readable plain text.
///
/// Comprehensive HTML-to-text converter: strips tags, skips script/style and
/// navigation-chrome blocks, keeps hyperlinks as `text (absolute-url)` and
/// table cells separated by ` | `, inserts newlines for block-level elements,
/// normalises whitespace, and decodes entities. Contrast with
/// `search_web_tool::strip_html_tags`, which is a lightweight tag stripper for
/// short search-result snippets.
///
/// `base_url` is the URL the body came from; relative `href`s resolve against
/// it. `fragment` is the `#fragment` that was asked for: if an element carries
/// a matching `id`, the text starts there instead of at the top of the page.
pub(crate) fn html_to_text(html: &str, base_url: Option<&str>, fragment: Option<&str>) -> String {
    let base = base_url.and_then(|url| reqwest::Url::parse(url).ok());
    let mut result = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_script = false;
    let mut in_style = false;
    let mut chrome_depth: usize = 0;
    let mut last_was_whitespace = false;
    let mut tag = String::new();
    // Open `<a href>` elements: the URL to print, and how much text had been
    // written when the link opened — a link with no text gets no URL.
    let mut open_links: Vec<(String, usize)> = Vec::new();
    let mut fragment_start: Option<usize> = None;
    // The hidden element being skipped, and how deeply elements of its name
    // nest inside it, so the skip ends at its own closing tag.
    let mut hidden: Option<(String, usize)> = None;
    // Inside a table cell, block elements (a `<p>` or `<div>` per cell is
    // common) must not break the row onto one line per value.
    let mut in_cell = false;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag.clear();
            continue;
        }

        if in_tag {
            if ch != '>' {
                tag.push(ch);
                continue;
            }
            in_tag = false;

            let name = tag_name(&tag);
            let closing = name.starts_with('/');
            let element = name.trim_start_matches('/');

            // Inside a hidden element, only track where it ends. One whose
            // end tag HTML lets a page leave out also ends where a browser
            // would close it, and this tag is then read as usual; otherwise
            // an unclosed hidden `<p>` or `<li>` would hide the rest of the
            // page.
            if let Some((hidden_name, 1)) = hidden.as_ref()
                && implicitly_closes(hidden_name, element, closing)
            {
                hidden = None;
            }
            if let Some((hidden_name, depth)) = hidden.as_mut() {
                if element == hidden_name.as_str() {
                    if closing {
                        *depth -= 1;
                    } else if !tag.ends_with('/') {
                        *depth += 1;
                    }
                }
                if *depth == 0 {
                    hidden = None;
                }
                continue;
            }

            // Blocks whose content is skipped entirely
            match element {
                "script" => in_script = !closing,
                "style" => in_style = !closing,
                _ if CHROME_TAGS.contains(&element) => {
                    chrome_depth = if closing {
                        chrome_depth.saturating_sub(1)
                    } else {
                        chrome_depth + 1
                    };
                }
                _ => {}
            }
            if in_script || in_style || chrome_depth > 0 {
                continue;
            }
            if !closing
                && !VOID_TAGS.contains(&element)
                && !tag.ends_with('/')
                && hides_element(&tag)
            {
                hidden = Some((element.to_string(), 1));
                continue;
            }

            // Where the requested #fragment starts, if this is its element
            if let Some(fragment) = fragment
                && fragment_start.is_none()
                && !closing
                && attr_value(&tag, "id").as_deref() == Some(fragment)
            {
                fragment_start = Some(result.len());
            }

            // Hyperlinks: `text (absolute-url)`
            if element == "a" {
                if closing {
                    if let Some((href, opened_at)) = open_links.pop()
                        && !result[opened_at..].trim().is_empty()
                    {
                        result.truncate(result.trim_end_matches(' ').len());
                        result.push_str(&format!(" ({href})"));
                        last_was_whitespace = false;
                    }
                } else if let Some(href) =
                    attr_value(&tag, "href").and_then(|href| resolve_href(base.as_ref(), &href))
                {
                    open_links.push((href, result.len()));
                }
            }

            // Table cells: keep neighbouring cells apart whatever whitespace
            // the source HTML happens to have between them
            match element {
                "td" | "th" => in_cell = !closing && !tag.ends_with('/'),
                "tr" | "table" => in_cell = false,
                _ => {}
            }
            if !closing && matches!(element, "td" | "th") {
                let line = result.trim_end_matches(' ');
                if !line.is_empty() && !line.ends_with('\n') {
                    result.truncate(line.len());
                    result.push(CELL_BREAK);
                    last_was_whitespace = true;
                }
            }

            // Add line breaks for block-level elements
            let is_block = matches!(
                element,
                "p" | "div"
                    | "br"
                    | "br/"
                    | "h1"
                    | "h2"
                    | "h3"
                    | "h4"
                    | "h5"
                    | "h6"
                    | "li"
                    | "tr"
                    | "blockquote"
                    | "pre"
                    | "hr"
                    | "header"
                    | "footer"
                    | "section"
                    | "article"
                    | "nav"
                    | "main"
            );
            if is_block && in_cell {
                // A break that `resolve_cell_breaks` settles once the whole
                // row is known: a space on a table row, a newline on a page
                // laid out as one big cell.
                if !result.ends_with([SOFT_BREAK, CELL_BREAK, '\n']) {
                    result.push(SOFT_BREAK);
                    last_was_whitespace = true;
                }
            } else if is_block && !result.ends_with('\n') {
                result.push('\n');
                last_was_whitespace = true;
            }
            continue;
        }

        // Skip content inside script, style, navigation-chrome and hidden blocks
        if in_script || in_style || chrome_depth > 0 || hidden.is_some() {
            continue;
        }

        // Normalize whitespace
        if ch.is_whitespace() {
            if !last_was_whitespace {
                result.push(' ');
                last_was_whitespace = true;
            }
        } else {
            result.push(ch);
            last_was_whitespace = false;
        }
    }

    // Start at the requested #fragment when the page had it
    if let Some(offset) = fragment_start {
        result.drain(..offset);
    }

    // Decode HTML entities, once (`html_entities::decode_html_entities`)
    let result = crate::tools::html_entities::decode_html_entities(&result);
    let result = resolve_cell_breaks(&result);
    let result = tidy_table_rows(&result);

    // Trim each line (a decoded `&nbsp;` spacer leaves lines of only
    // whitespace) and keep at most one blank line in a row
    let mut cleaned = String::with_capacity(result.len());
    let mut blank_run = 0;
    for line in result.split('\n').map(str::trim) {
        if line.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }

    cleaned.trim().to_string()
}
