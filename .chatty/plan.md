# Plan: remember tool char-boundary fix (AGE-519)

Acceptance criteria:
- remember_tool.rs only: private `confirmation(title, content)` using floor_char_boundary(80)
- `call` returns Ok(confirmation(...)); no `content.len().min(80)` remains
- two tests in `mod tests` at file end
- memory_query.rs change (bfe55341) NOT taken

Steps:
- [x] 1 bring in remember_tool.rs change from existing work (d9727d7c; memory_query.rs reverted to origin/main) - committed on this branch (see git log)
- [ ] 2 verify checks (cargo test remember_tool, cargo fmt --check)
