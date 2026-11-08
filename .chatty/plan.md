# AGE-533 plan

## Acceptance criteria
- With exec_settings == None, none of fetch, search_web, browser open-web access, browser_use, daytona_run is enabled.
- With settings present, behaviour unchanged (follows fetch_enabled).
- New test passes; `factories::agent_factory` and `session::` tests still pass.
- clippy -D warnings and cargo fmt --all --check pass.
- Only crates/chatty-core/src/factories/agent_factory/mod.rs changes (plus .chatty state files, removed later).

## Steps
1. [ ] Add `internet_access_allowed` helper next to `doc_retriever_enabled`.
2. [ ] Replace the five `fetch_enabled.unwrap_or(true)` expressions with the helper.
3. [ ] Add test `internet_access_needs_execution_settings_with_fetch_on`.
4. [ ] Verify `git grep "s.fetch_enabled"` shows only the helper; run tests, clippy, fmt.
5. [ ] Commit.
