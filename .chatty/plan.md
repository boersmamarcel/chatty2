# Plan: age-528 verify

## Acceptance criteria
- The unknown-tool nudge (UNKNOWN_TOOL_CALL_FOLLOW_UP_PREFIX) is recognised as a one-shot recovery.
- The nudge is sent once, not a second time on its own follow-up.
- Change is limited to chatty-core session handler/mod/tests.
- Unit test passes; clippy and fmt are clean.

## Steps
- [x] 1. Confirm diff vs origin/main (3 files: session/handler.rs, session/mod.rs, session/tests.rs)
- [x] 2. Confirm code (prefix const exported; included in already_asked_to_retry list)
- [ ] 3. Run test (not run in this task)
- [ ] 4. Run clippy + fmt (not run in this task)
