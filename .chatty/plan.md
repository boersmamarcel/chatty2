# AGE-1095: cap the page body read in search_web

## Acceptance criteria
- search_web reads at most MAX_PAGE_BYTES of a result page body; the rest is never buffered.
- New test passes; AGE-532 redirect tests and other tools::search_web_tool tests still pass.
- cargo clippy -p chatty-core --all-targets -- -D warnings and cargo fmt --all --check pass.
- Only crates/chatty-core/src/tools/search_web_tool.rs changes (plus .chatty state files).

## Steps
1. [ ] Use read_text_capped in get_following_checked_redirects, add test (commit: pending)
