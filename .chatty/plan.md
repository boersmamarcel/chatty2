# AGE-1095: search_web reads at most MAX_PAGE_BYTES of a page body

## Acceptance criteria
- search_web reads at most MAX_PAGE_BYTES of a result page body; the rest is never buffered.
- New test `page_fetch_stops_reading_at_the_page_cap` passes; other `tools::search_web_tool` tests pass.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only `search_web_tool.rs` changes (besides these state files).

## Steps
1. [x] In `get_following_checked_redirects`, read the body with `fetch_tool::read_text_capped(response, MAX_PAGE_BYTES)`; document it; add the test. (commit: see git log, subject "search_web: cap page body read at MAX_PAGE_BYTES")
