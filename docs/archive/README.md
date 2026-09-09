# Archived notes

Point-in-time plans, audits and bug lists that were useful while the work
was in flight. They are kept for their reasoning and code references, but
they are **not documentation**: nothing here is synced to the docs site,
listed in `docs/INDEX.md`, or expected to match the current code.

Open items from these files live in Linear (Chatty tech debt project), not
here. If you need one of these documents to be true again, rewrite it as an
architecture page under `docs/` instead of editing it in place.

| File | What it was | Where the open items went |
|---|---|---|
| `chatty-bugs-plan.md` | Remediation plan for the Chatty bugs project (all workstreams shipped) | Chatty bugs project |
| `message-path-debt.md` | Hot-path audit of send → stream → persist at `df14649` (2026-09-05) | AGE-270 |
| `refactor-followups.md` | Leftovers from the large-file split refactor | AGE-271, AGE-173 |
| `pre-built-apis.md` | Hand-kept inventory of `#[allow(dead_code)]` surface | `cargo` warnings; AGE-175 |
| `research/promotion-log.md` | Empty template for research promotion verdicts | Self-improving chatty2 project |
| `research/crate-promises-*.md` | Scope statements for the research crates while they were stubs | Each crate's `README.md` |
