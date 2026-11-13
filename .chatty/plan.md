# Plan: prove fetch_tool split is a pure move

Acceptance criteria:
- git diff --stat -M origin/main...HEAD: only fetch_tool.rs removed (-3594) and five fetch_tool/*.rs added
- Only non-moved added lines: mod decls, use lines, pub(super)/pub(crate), `pub(crate) use html::html_to_text;`
- Bodies of html_to_text, find_passages, window, error_body_window and tests identical to origin/main
- Each file <= 1200 lines (html 516, mod 1135, passages 237, tests 819, tests_net 899)
- 86 test attrs on main == 86 in tests.rs+tests_net.rs
- cargo fmt --all --check exits 0
- mod.rs contains `pub(crate) use html::html_to_text;`

Steps:
1. [x] Verify the split is a pure move and record findings (see progress.md)
