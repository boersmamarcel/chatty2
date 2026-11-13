# AGE-723: split fetch_tool.rs
1. [x] Record baseline test count (87) in progress.md
2. [x] git mv fetch_tool.rs -> fetch_tool/mod.rs; passages.rs, html.rs; tests -> tests.rs + tests_net.rs
3. [x] Re-export html_to_text; visibility pub(super) only where needed
4. [x] Run checks, wc -l, test count equal
