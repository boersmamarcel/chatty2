# AGE-1094: cap fetch's text body at 5 MiB

## Acceptance criteria
- fetch's success text path reads at most 5 MiB; longer page is cut and output carries TEXT_CUT_NOTE.
- Pages decoded in declared charset (UTF-8 default).
- 3 new tests pass, existing tools::fetch_tool tests pass.
- clippy -D warnings and cargo fmt --all --check pass.
- Only fetch_tool.rs, crates/chatty-core/Cargo.toml, Cargo.lock change (plus .chatty state files); Cargo.lock only gains "encoding_rs" in chatty-core's dependency list.

## Steps
1. [x] (commit: code commit, see git log) Code: encoding_rs dep, MAX_TEXT_BYTES/TEXT_CUT_NOTE, decode_text, read_text_capped, use in read_response.
2. [ ] Tests: decode_text, read_text_capped cap, read_text_capped short body; run checks.
