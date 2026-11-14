# AGE-1389 plan (part of AGE-1387)

Acceptance criteria:
- empty `depends-on:` parses with empty depends_on
- empty `conflict-keys:` parses with empty list
- `depends-on: ,`, repeated header, unknown id and `ship: yes` are still errors
- only crates/chatty-core/src/coding.rs changes (plus these state files)

Steps:
1. [x] write failing tests (faff9cd8)
2. [x] fix parse_items (addff0ed)
3. [x] run checks (cargo test -p chatty-core --lib coding; clippy; fmt) (addff0ed)
