---
name: fix-ci
description: Diagnoses and fixes CI pipeline failures for Chatty. Analyzes GitHub Actions workflow logs, identifies root causes, and applies fixes. Use when CI checks are failing on a PR or branch.
argument-hint: "[pr-number-or-run-id]"
allowed-tools: Bash, Read, Grep, Glob, Edit
---

# Fix CI

Diagnoses and fixes CI failures for the Chatty project.

## Gathering Failure Context

### If a PR number is provided:

```bash
# Get failed checks
gh pr checks $0

# Get the latest failed run
gh run list --branch $(gh pr view $0 --json headRefName -q '.headRefName') --status failure --limit 1

# View run logs
gh run view <run-id> --log-failed
```

### If a run ID is provided:

```bash
gh run view $0 --log-failed
```

### If no argument provided:

```bash
# Check current branch
gh run list --branch $(git branch --show-current) --status failure --limit 1
```

## CI Pipeline Structure

The Chatty CI pipeline (`ci.yml`) runs three checks on pull requests to `main`:

1. **cargo test --all-features** — Unit and integration tests
2. **cargo fmt --check** — Code formatting validation
3. **cargo clippy -- -D warnings** — Lint checks (warnings = errors)

## Diagnosing Common Failures

### Test Failures

1. Identify the failing test from the log output
2. Read the test source file to understand what it tests
3. Check if the failure is:
   - A legitimate bug introduced by the PR changes
   - A flaky test (check if it passes locally)
   - A test that needs updating due to intentional behavior changes

### Formatting Failures

1. Run `cargo fmt` locally to auto-fix
2. Stage and commit the formatting changes
3. Common causes: new code not formatted, editor auto-format disabled

### Clippy Failures

1. Read the clippy warning message carefully
2. Common warnings in this codebase:
   - `unused_imports` — Remove unused `use` statements
   - `dead_code` — Remove or use the unused code
   - `needless_return` — Use implicit returns
   - `clone_on_copy` — Don't clone types that implement `Copy`
   - `single_match` — Convert single-arm `match` to `if let`
3. Apply the fix suggested by clippy (usually shown in the output)

### Build Failures

1. Check for missing dependencies or incompatible versions
2. Verify `Cargo.lock` is committed and up to date
3. Check if new system dependencies are needed (Linux packages in CLAUDE.md)

## Fix Workflow

1. **Reproduce locally** — Run the failing command on the current branch
2. **Apply the fix** — Make the minimum change needed
3. **Verify locally** — Re-run the failing command to confirm the fix
4. **Run full pipeline** — Run all three checks to ensure no regressions:
   ```bash
   cargo test --all-features && cargo fmt --check && cargo clippy -- -D warnings
   ```
5. **Commit the fix** with a descriptive message referencing the CI failure
