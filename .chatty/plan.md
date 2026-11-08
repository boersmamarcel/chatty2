# AGE-529: a worker with no base is measured against the workspace's branch

## Acceptance criteria
- `add_worktree` returns `WorkerTree.base` = given base, else the workspace's current branch (None on detached HEAD).
- Docs of `WorkerTree::base` and the module doc say so.
- Test `nested_worker_of_a_treeless_sub_leader_starts_at_head` expects base `main`.
- New test `a_worker_on_a_feature_branch_is_measured_against_that_branch` passes.
- Only worker_tree.rs and .chatty files change.

## Steps
1. [x] Implement change, docs, tests in worker_tree.rs; run checks; commit (commit: this one; see git log)
