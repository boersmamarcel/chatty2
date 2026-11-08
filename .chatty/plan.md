# AGE-532 plan: SSRF-check every redirect hop in SearchWebTool::page_text

## Acceptance criteria
- page_text never requests a URL that has not passed check_public_host, including every redirect target.
- Redirects followed by get_following_checked_redirects, max 5 hops; page_client follows none.
- 3 new tests pass; existing tools::search_web_tool tests still pass.
- `cargo clippy -p chatty-core --all-targets -- -D warnings` and `cargo fmt --all --check` pass.
- Only search_web_tool.rs changes (plus .chatty state).

## Steps
1. [x] Implementation: page_client field, MAX_PAGE_REDIRECTS, get_following_checked_redirects, page_text change (commit: see git log, "AGE-532")
2. [ ] Tests (3) in mod tests
3. [ ] Run checks: cargo test -p chatty-core --lib tools::search_web_tool; clippy; fmt
