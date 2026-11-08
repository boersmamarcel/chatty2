# Plan: AGE-550 – each SQLite migration and its version bump in one transaction

## Acceptance criteria
- New test `a_failed_migration_leaves_no_half_applied_schema` passes.
- The 3 existing tests in conversation_sqlite_repository still pass.
- A failed migration leaves no partial change and no version bump.
- Re-running the full MIGRATIONS list afterwards succeeds.
- clippy -D warnings and fmt pass.
- Only conversation_sqlite_repository.rs (plus .chatty state files) changes.

## Steps
1. [ ] Wrap each pending migration + version bump in a transaction, update doc comment, add test (commit: pending)
