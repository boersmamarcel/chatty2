## 2026-11-14
- Step 1: added 4 tests in coding.rs. Before the fix, empty_depends_on_is_no_dependency and empty_conflict_keys_is_no_keys FAIL (empty list -> BadHeader); comma_only_depends_on_is_still_bad and repeated_empty_header_is_still_bad pass. A first foreground cargo run timed out (slow build); reran in background.
- Step 2: list() in item_of returns Ok(vec![]) when the value is empty after trim; separator-only values, repeats, unknown ids and bad ship values unchanged.
- Step 3: cargo test -p chatty-core --lib coding: 24 passed, 0 failed; clippy -D warnings: clean; fmt --check: clean.
