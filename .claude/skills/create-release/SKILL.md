---
name: create-release
description: Prepares and creates a new release for Chatty. Handles version bumping in Cargo.toml, creating git tags, and triggering the release workflow. Use when the user wants to release a new version.
argument-hint: "[patch|minor|major]"
allowed-tools: Bash, Read, Grep, Glob, Edit
---

# Create Release

Prepares and executes a new Chatty release. The bump type is `$ARGUMENTS` (defaults to `patch` if not specified).

**Human path only.** Do not use this skill for Linear **Chatty auto-ship** / `ship:auto`
work — that path labels the PR `ship:auto` + `release:patch` and relies on auto-merge +
`prepare-release`. This skill is for Marcel-driven releases (PR label or `workflow_dispatch`).

## Determine Release Method

First, figure out the current context:

```bash
BRANCH=$(git branch --show-current)
echo "Current branch: $BRANCH"
```

Then determine the bump type from `$ARGUMENTS`:
- If `$ARGUMENTS` is `patch`, `minor`, or `major` → use that
- If `$ARGUMENTS` looks like a version number (e.g. `0.1.53`) → use as explicit version
- If `$ARGUMENTS` is empty → default to `patch`

---

### If on a PR branch (not `main`):

The preferred flow is to **add a release label** to the PR. When the PR is merged, the `prepare-release` workflow triggers automatically.

1. **Find the PR number for the current branch**:
   ```bash
   PR_NUMBER=$(gh pr view --json number -q .number 2>/dev/null)
   ```

2. **Add the release label** (e.g. `release:patch`, `release:minor`, or `release:major`):
   ```bash
   gh pr edit "$PR_NUMBER" --add-label "release:<bump_type>"
   ```
   The label will be created automatically if it doesn't exist yet.

3. **Confirm to the user**: Tell them the label has been added and that merging the PR will automatically trigger the full release pipeline (version bump, changelog, tag, GitHub release, cross-platform builds).

---

### If on `main` (no PR context):

Fall back to triggering the workflow directly.

1. **Pre-flight checks**:
   ```bash
   git status --porcelain          # Must be clean
   cargo test --all-features 2>&1  # Tests pass
   cargo clippy -- -D warnings 2>&1  # Lints pass
   ```

2. **Trigger the prepare-release workflow**:
   ```bash
   gh workflow run prepare-release.yml \
     -f bump=<bump_type>
   ```

   Or with an explicit version:
   ```bash
   gh workflow run prepare-release.yml \
     -f bump=patch \
     -f version_override=<version>
   ```

3. **Verify the workflow started**:
   ```bash
   # Wait a moment for the run to register
   sleep 3
   gh run list --workflow=prepare-release.yml --limit=1
   ```

## What Happens Next

The `prepare-release` workflow handles everything in a single run:
1. Bumps version in `Cargo.toml` + `Cargo.lock`
2. Generates a categorized changelog from commits since last tag
3. Opens (and squash-merges) a `cut-release` PR — `main` is protected, so it does not `git push origin main`
4. Creates an annotated tag and a GitHub Release
5. Calls `release.yml` directly via `workflow_call` to build artifacts (no event-based handoff)

The build pipeline produces:
- **Linux x86_64**: `chatty-linux-x86_64.AppImage`
- **macOS ARM**: `chatty-macos-aarch64.dmg` (code-signed if secrets configured)
- **Windows x86_64**: `chatty-windows-x86_64.exe` (Inno Setup installer)
- **Checksums**: `checksums.txt` (SHA-256 for all artifacts)

## Post-Release Verification

After the workflow completes:

```bash
# Check release workflow status
gh run list --workflow=release.yml --limit=1

# View the release
gh release list --limit=1
```

## Troubleshooting

- **Label not triggering**: Ensure the PR targets `main` and has exactly one of `release:patch`, `release:minor`, `release:major`
- **GH006 / protected main**: Expected if a workflow still `git push origin main`. Current `prepare-release` lands the bump via a `cut-release` PR instead. Do not hand-create tags.
- **Version mismatch error**: The prepare-release workflow handles this automatically — no manual version matching needed
- **Build failures**: Check platform-specific logs in GitHub Actions
