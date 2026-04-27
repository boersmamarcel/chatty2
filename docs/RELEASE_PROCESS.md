# Release Process

This document explains how to create a release for Chatty.

## Prerequisites

1. Version in `Cargo.toml` matches the release version you want to create
2. All changes are committed and pushed to `main`

## Release Workflow Options

The release pipeline supports **two workflows**:

### Option 1: Tag Push (Automatic)

**Best for:** Quick releases without custom release notes

```bash
# 1. Update version in Cargo.toml
vim Cargo.toml  # Set version = "0.1.21"

# 2. Commit and push
git add Cargo.toml
git commit -m "Bump version to 0.1.21"
git push origin main

# 3. Create and push tag
git tag v0.1.21
git push origin v0.1.21
```

**What happens:**
- Workflow triggers on tag push
- Validates version matches Cargo.toml
- Builds for Linux, macOS, Windows
- Creates a **published** (not draft) release automatically
- Generates release notes from commits
- Uploads all build artifacts

### Option 2: GitHub UI Release (Manual)

**Best for:** Releases with custom release notes or announcements

1. **Update version in Cargo.toml**
   ```bash
   vim Cargo.toml  # Set version = "0.1.21"
   git add Cargo.toml
   git commit -m "Bump version to 0.1.21"
   git push origin main
   ```

2. **Create release via GitHub UI:**
   - Go to: https://github.com/boersmamarcel/chatty2/releases/new
   - Choose tag: `v0.1.21` (or create new tag)
   - Target: `main` branch
   - Release title: `v0.1.21` or custom title
   - Description: Write custom release notes
   - **Important:** Click "Publish release" (NOT "Save as draft")

3. **Workflow runs automatically:**
   - Triggered by the `published` release event
   - Builds for all platforms
   - Uploads artifacts to your release
   - Preserves your custom release notes
   - Publishes the release (overrides draft status if needed)

## What Gets Built

Each release includes:

- **Linux**: `chatty-linux-x86_64.AppImage`
- **macOS**: `chatty-macos-aarch64.dmg` (ARM, code-signed if secrets configured)
- **Windows**: `chatty-windows-x86_64.exe` (Inno Setup installer)
- **Checksums**: `checksums.txt` (SHA-256 for all files)

## Version Validation

The workflow **validates** that:
- Tag version (e.g., `v0.1.21` → `0.1.21`) matches `Cargo.toml` version
- If they don't match, the workflow **fails** with an error

**Fix version mismatches:**
```bash
# If you accidentally created tag v0.1.21 but Cargo.toml has 0.1.20:

# Option 1: Update Cargo.toml and re-tag
vim Cargo.toml  # Change to 0.1.21
git add Cargo.toml
git commit -m "Fix version mismatch"
git push origin main
git tag -d v0.1.21           # Delete local tag
git push origin :v0.1.21     # Delete remote tag
git tag v0.1.21              # Recreate tag
git push origin v0.1.21

# Option 2: Use correct version tag
git tag v0.1.20              # Tag matching Cargo.toml
git push origin v0.1.20
```

## Code Signing (macOS)

See [scripts/setup-codesigning.md](../scripts/setup-codesigning.md) for details.

**Quick summary:**
- Without secrets: Ad-hoc signed (works locally, Gatekeeper warnings)
- With secrets: Developer ID signed (no warnings, notarization optional)

Required secrets (optional):
- `MACOS_CERTIFICATE` - Base64 P12 certificate
- `MACOS_CERTIFICATE_PASSWORD` - P12 password
- `KEYCHAIN_PASSWORD` - Random keychain password
- `MACOS_SIGNING_IDENTITY` - Certificate name

## How Duplicate Workflows Are Prevented

**Problem:** When you create a release via GitHub UI, it triggers **BOTH**:
- `release.published` event (from publishing the release)
- `push.tags` event (from creating the tag)

This caused two workflows to run simultaneously, racing to upload assets to the same release, resulting in "Too many retries" errors.

**Solution:** The workflow now:
1. Uses `concurrency` groups to queue workflows for the same tag
2. Checks if a release already exists before running (for tag push events)
3. Skips the tag-push workflow if a release was already created via UI

**Result:** Only one workflow runs per release, preventing conflicts.

## Troubleshooting

### "Too many retries" Error (Fixed)

**Symptom:** Workflow fails with "Too many retries" when uploading assets.

**Cause:** Multiple workflows running simultaneously for the same release (race condition).

**Fix:** This is now fixed with duplicate workflow detection. You should no longer see this error.

### Release stays as draft

**Old behavior (before fix):** If you created a draft release, the workflow would attach assets but not publish it.

**Fixed:** The workflow now always sets `draft: false` to ensure releases are published.

**If you still see a draft:**
- The workflow might have failed before reaching the upload step
- Check the Actions tab for errors
- Manually publish the draft after workflow completes

### Workflow fails at validation

**Error:** "Version mismatch! Tag version (X.Y.Z) does not match Cargo.toml version (A.B.C)"

**Fix:** See "Version Validation" section above

### Assets not uploaded

**Common causes:**
1. Build job failed (check Linux/macOS/Windows build logs)
2. Artifact upload failed (check artifact names match)
3. `fail_on_unmatched_files: true` - files missing

**Debug:**
- Check "Prepare release assets" step for warnings
- Check "Debug release assets" step for file list
- Ensure build scripts produced expected files

### macOS code signing fails

**Error:** "No identity found" or certificate import fails

**Check:**
- Secrets are correctly set (base64 encoding, no extra newlines)
- Certificate hasn't expired
- Keychain password is correct

**Fallback:** Remove/comment out the "Import code signing certificate" step to use ad-hoc signing

## Monitoring Releases

- **Actions:** https://github.com/boersmamarcel/chatty2/actions/workflows/release.yml
- **Releases:** https://github.com/boersmamarcel/chatty2/releases

## Best Practices

1. **Always update Cargo.toml first** before creating tags
2. **Use semantic versioning**: `MAJOR.MINOR.PATCH` (e.g., `0.1.21`, `1.0.0`)
3. **Tag format**: Use `v` prefix (`v0.1.21`) for consistency
4. **Release notes**: Use Option 2 (GitHub UI) for important releases with detailed notes
5. **Test locally first**: Run `cargo build --release` and `./scripts/package-macos.sh` before releasing
6. **Check Actions**: Monitor the workflow to ensure all platforms build successfully

## Future Improvements

- [ ] Add macOS Intel builds (currently only ARM)
- [ ] Add automatic changelog generation from commits
- [ ] Add release notification (Slack, Discord, etc.)
- [ ] Add auto-updater support (download latest version from GitHub)
- [ ] Add Windows code signing
