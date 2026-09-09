---
name: ship
description: Hand finished work on a Linear issue to the repo's automation so it lands without a human. Commits, pushes, opens the PR titled with the issue id, and arms auto-merge, which puts the PR in the one-lane queue and in the PR nurse's care. Use when a branch is verified and ready; use `release` for a patch that should also cut a release. Never for owner:human work or gate issues.
argument-hint: "<AGE-NNN> [release]"
allowed-tools: Bash, Read, Grep, Glob
---

# Ship

Turn the current branch into a PR that merges on its own. After this skill
runs, nothing on the PR needs a person: `update-next-queued-pr.yml` keeps the
branch current with `main`, `pr-nurse.yml` resolves conflicts and red required
checks (two attempts, then `blocked:human`), and GitHub auto-merge squashes it
when `test`, `check-reserved` and `guard` are green. With `release`, the
`ship:auto` path also cuts a patch release.

Arguments: `$ARGUMENTS` = the Linear issue id (`AGE-NNN`, required) and
optionally the word `release`.

## 1. Refuse when

- The issue is `owner:human`, or a `gate:reflection` / `gate:product-decision` issue.
- A symbol listed in `RESERVED.md` was implemented without a `// HUMAN-WRITTEN:` attestation. Run `bash scripts/check-reserved.sh`; it must pass.
- The branch is `main`, or the working tree carries changes that belong to another issue.
- Verification has not been run in this session. Run it now if unsure:
  ```bash
  cargo test --all-features -- --test-threads=1 && cargo fmt --check && \
  cargo clippy --all-features --all-targets -- -D warnings && bash scripts/check-reserved.sh
  ```
- `release` was asked but the diff touches release-guarded paths (`RESERVED.md`,
  `.cursor/rules/ownership.mdc`, `crates/chatty-trace/`, `chatty-playbook`,
  `chatty-flow`, `chatty-optimize`, `hive-billing-sdk`, auth or provider-key
  code). `ship-auto-guard.yml` fails those; do not try.

Say which rule refused and stop.

## 2. Branch name

The branch must carry the issue id in lowercase so Linear links the PR:
`age-nnn-<short-slug>`. It must **not** carry a parent or epic id: Linear
closes the issue named in the branch when the PR merges, and a branch named
after the parent closes the parent early and strands its children.

For `release`, the ship-auto contract needs an `auto/` prefix:
```bash
git branch -m auto/age-nnn-<slug>
```

## 3. Commit and push

One commit per logical change, imperative summary, the issue id at the end:
```bash
git add -A
git commit -m "<what changed, imperative> (AGE-NNN)

<why, two or three lines>

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
git push -u origin HEAD
```

## 4. Open the PR

Title: `<summary> (AGE-NNN)`. Body, in this shape:

```markdown
Closes AGE-NNN · <linear issue url>

<what and why, a short paragraph>

## Verification
<the exact commands run and their result lines>

## Acceptance criteria
| Criterion (from the issue) | Evidence |
| -- | -- |
| ... | test name, file, or command output |

🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

```bash
gh pr create --title "<summary> (AGE-NNN)" --body-file /tmp/pr-body.md
```

For `release`, add `--label ship:auto --label release:patch`. Never add
`ship:auto` without `release:patch`, and never `release:minor` or `major` on
this path.

## 5. Arm auto-merge

```bash
gh pr merge --auto --squash <pr-number>
```

This is the step that puts the PR in care. For `release` the
`ship-auto-merge` workflow also arms it; arming twice is harmless.

## 6. Report and leave

Print the PR URL and stop. Do not wait for CI, do not merge by hand, do not
re-run checks. If CI goes red or `main` moves, the nurse and the queue handle
it; if they give up, the PR gets `blocked:human` and a comment, which is the
signal for a person.
