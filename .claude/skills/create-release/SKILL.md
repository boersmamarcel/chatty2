---
name: create-release
description: Prepares and creates a new release for Chatty. Handles version bumping in Cargo.toml, creating git tags, and triggering the release workflow. Use when the user wants to release a new version.
argument-hint: "[version]"
allowed-tools: Bash, Read, Grep, Glob, Edit
---

# Create Release

Prepares and executes a new Chatty release. The target version is `$ARGUMENTS`.

## Pre-flight Checks

Before starting, verify:

1. **Working tree is clean**:
   ```bash
   git status --porcelain
   ```

2. **On the main branch**:
   ```bash
   git branch --show-current
   ```

3. **Current version** in Cargo.toml:
   ```bash
   grep '^version' Cargo.toml | head -1
   ```

4. **Build succeeds** (optional — can be skipped if you trust CI, since the release workflow will catch build failures):
   ```bash
   cargo build --release 2>&1
   ```

5. **Tests pass**:
   ```bash
   cargo test --all-features 2>&1
   ```

6. **Lints pass**:
   ```bash
   cargo clippy -- -D warnings 2>&1
   ```

If any check fails, stop and report the issue.

## Version Bump

1. Update the `version` field in `Cargo.toml`
2. Use semantic versioning: `MAJOR.MINOR.PATCH`
   - PATCH: Bug fixes, minor improvements
   - MINOR: New features, non-breaking changes
   - MAJOR: Breaking changes

If no version is provided via `$ARGUMENTS`, suggest the next patch version based on the current version.

## Release Steps

### Option 1: Tag Push (Recommended for standard releases)

```bash
# 1. Commit version bump
git add Cargo.toml Cargo.lock
git commit -m "Bump version to <version>"
git push origin main

# 2. Create and push tag
git tag v<version>
git push origin v<version>
```

### Option 2: GitHub UI Release (For releases with custom notes)

1. Commit and push the version bump as above
2. Direct the user to create the release via GitHub UI at:
   `https://github.com/boersmamarcel/chatty2/releases/new`
3. Tag: `v<version>`, Target: `main`

## Post-Release Verification

After pushing the tag:

1. Check that the release workflow started:
   ```bash
   gh run list --workflow=release.yml --limit=1
   ```

2. Monitor the workflow:
   ```bash
   gh run watch
   ```

3. Verify the release was created:
   ```bash
   gh release view v<version>
   ```

## What Gets Built

The release pipeline (`.github/workflows/release.yml`) builds:
- **Linux x86_64**: `chatty-linux-x86_64.AppImage`
- **macOS ARM**: `chatty-macos-aarch64.dmg` (code-signed if secrets configured)
- **Windows x86_64**: `chatty-windows-x86_64.exe` (Inno Setup installer)
- **Checksums**: `checksums.txt` (SHA-256 for all artifacts)

## Troubleshooting

- **Version mismatch error**: Tag version must match Cargo.toml version exactly
- **Duplicate workflow**: Concurrency groups prevent race conditions; only one workflow runs per tag
- **Build failures**: Check platform-specific logs in GitHub Actions
