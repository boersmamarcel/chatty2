# Plan: repo-root allowlist check (AGE-720)

## Acceptance criteria
- scripts/check-repo-root.sh passes on the repo, fails naming each stray, exits 1.
- `--self-test` proves pass / fail (foo.py) / pass (dotfile) in a throwaway repo.
- Mutations (no `exit 1`, no dot exception) make the self-test fail.

## Steps
1. [x] Write scripts/check-repo-root.sh with allowlist and --self-test, verify mutations (commit: see git log, "Add repo root allowlist check script")
