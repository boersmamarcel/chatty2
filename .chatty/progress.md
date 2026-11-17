# Progress

## 2026-11-17 - step 1
Changed the `add_attachment` tool-list line in `preamble_builder.rs` to
"show an image to the user: to show a screenshot, deliver it with this tool".
Added test `add_attachment_says_it_shows_images_and_how_to_show_a_screenshot`
(the old phrase is built with `concat!`, so it is absent from the file).
Checks: `cargo fmt --all --check` OK; `cargo clippy -p chatty-core --all-targets -- -D warnings` OK;
`cargo test -p chatty-core --lib preamble_builder` 32 passed, 0 failed.
