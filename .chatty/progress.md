
## 2026-11-09 — AGE-1097
- Commit 3dcd52431a9d (tester-1 mutation check) wrongly landed on this branch, reverting `.create_new(true)` to `.create(true).truncate(true)` in crates/chatty-core/src/tools/fetch_tool.rs.
- Reverted it (no history rewrite); fetch_tool.rs matches cd6093940a64 again.
