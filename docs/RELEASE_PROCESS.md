# Release Process

**When to read this:** You are about to cut a Chatty release, or a release run did not
produce the artifacts you expected.

## Closed-system security contract

Only **`github.repository_owner`** and **`github-actions[bot]`** may arm releases.
Forks and outside collaborators cannot:

- Dispatch `release.yml` / `prepare-release.yml` manually
- Apply privileged labels (`ship:auto`, `release:patch|minor|major`, `cut-release`)
- Enable ship-auto auto-merge
- Trigger prepare-release from a fork PR head

Linear projects **Chatty auto-ship** and **Chatty tech debt** membership is Marcel
(and agents he runs). Weekly dependency-check filings go to Linear only.

**Emergency rebuild** (owner only), when a published release event did not fire.
Dispatch **against the tag ref** (never from `main` with only an input):

```bash
gh workflow run release.yml --ref vX.Y.Z -f tag_name=vX.Y.Z
```

`release.yml` verifies the checked-out commit matches `refs/tags/vX.Y.Z`, asserts that
commit is an ancestor of `origin/main`, and matches `Cargo.toml` before building.

Authz gate regression check: `bash scripts/check-release-authz.sh` (also run in CI).

## Preferred paths (use these)

### A. PR merge with a release label (human path)

1. Open a PR to `main` with exactly one of `release:patch`, `release:minor`, `release:major`
   (owner-applied; unauthorized labelers are stripped by `privileged-labels.yml`).
2. Merge when CI is green. `ship-auto-guard` enforces the path deny-list on any
   release-labeled PR.
3. `prepare-release.yml` bumps `Cargo.toml` on a `release/vX.Y.Z` PR (`cut-release`,
   not `release:patch`), merges that PR (main is protected — a direct push gets GH006),
   generates the changelog, tags `vX.Y.Z`, creates the GitHub Release with that
   changelog as its body, and calls `release.yml` via `workflow_call` to build artifacts.
   Runs are serialised by a `prepare-release` concurrency group so two merges cannot
   race on the version bump or the tag.

On a PR branch, `/create-release patch` (or minor/major) only **adds the label** — it does
not create a tag by hand.

### B. Auto-ship (zero-human patch releases)

For low-risk work filed in Linear project **Chatty auto-ship** or **Chatty tech debt**
(with `ship:auto` + `owner:ai`, not `owner:human`):

1. Agent opens PR: branch `auto/*`, title `auto: …`, labels `ship:auto` + `release:patch`
   (owner/Actions only).
2. `ship-auto-guard.yml` enforces patch-only + path deny-list; `ship-auto-merge.yml`
   enables squash auto-merge only for same-repo heads when the sender is the owner or
   the actor is `github-actions[bot]`. Later `synchronize` events from other same-repo
   actors do not re-arm; if auto-merge is already on, the prepare-release waiter still
   starts.
3. When required checks are green, auto-merge squash-merges to `main`.
4. If Actions performed the squash (`GITHUB_TOKEN` does not emit
   `pull_request.closed`), `ship-auto-merge` dispatches `prepare-release`
   (`bump=patch`). Owner merges still use the `pull_request` closed path.
5. Same `prepare-release` → bump PR → tag → `release.yml` pipeline as (A).

Never hand-tag for auto-ship.

## Deprecated for routine releases: GitHub UI release

`release.yml` has three triggers: the `workflow_call` from Prepare Release, an
owner-only `workflow_dispatch` (the emergency rebuild above), and the `published`
event of a GitHub Release. There is **no tag-push trigger**: pushing a `vX.Y.Z` tag by
hand builds nothing.

Publishing a release from the GitHub UI still works and is the way to ship custom
release notes, but hand-editing `Cargo.toml` is easy to desync from the tag. Prefer
(A) or (B).

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
   - **Important:** Click "Publish release" (NOT "Save as draft") — only the
     `published` event triggers the workflow

3. **Workflow runs automatically:**
   - Validates the tag against `Cargo.toml` and `origin/main`
   - Builds for all platforms
   - Uploads artifacts to your release, preserving your release notes

## What Gets Built

Each release includes:

- **Linux**: `chatty-linux-x86_64.AppImage`
- **macOS**: `chatty-macos-aarch64.dmg` (ARM, code-signed if secrets configured)
- **Windows**: `chatty-windows-x86_64.exe` (Inno Setup installer)
- **Checksums**: `checksums.txt` (SHA-256 for all files)

The desktop app's auto-updater (`crates/chatty-gpui/src/auto_updater/`) polls the
GitHub releases API for these assets, so a published release reaches existing
installs without any further step.

## Version Validation

The workflow **validates** that:
- Tag version (e.g., `v0.1.21` → `0.1.21`) matches `Cargo.toml` version
- The tag commit is an ancestor of `origin/main`
- If either check fails, the workflow **fails** with an error

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

See [scripts/setup-codesigning.md](https://github.com/boersmamarcel/chatty2/blob/main/scripts/setup-codesigning.md) for details.

**Quick summary:**
- Without secrets: Ad-hoc signed (works locally, Gatekeeper warnings)
- With secrets: Developer ID signed (no warnings, notarization optional)

Required secrets (optional):
- `MACOS_CERTIFICATE` - Base64 P12 certificate
- `MACOS_CERTIFICATE_PASSWORD` - P12 password
- `KEYCHAIN_PASSWORD` - Random keychain password
- `MACOS_SIGNING_IDENTITY` - Certificate name

## Troubleshooting

### Workflow fails at validation

**Error:** "Version mismatch! Tag version (X.Y.Z) does not match Cargo.toml version (A.B.C)"
or "Tag … is not an ancestor of origin/main"

**Fix:** See "Version Validation" section above

### Assets not uploaded

**Common causes:**
1. Build job failed (check Linux/macOS/Windows build logs)
2. Artifact upload failed (check artifact names match)
3. `fail_on_unmatched_files: true` - files missing
4. Prepare Release created the GitHub Release but `release.yml` did not build.
   Reusable workflows inherit the caller's `github.event_name`, so the reusable-call
   path is marked with an explicit `from_prepare: true` input rather than keyed off
   the event name.

**Rebuild an existing tag** (owner only; runs the workflow from that tag):

```bash
gh workflow run release.yml --ref vX.Y.Z -f tag_name=vX.Y.Z
```

**Debug:**
- Check the "Prepare release assets" step for warnings and the file list
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

1. **Let Prepare Release bump the version** — hand-edit `Cargo.toml` only for a UI release
2. **Use semantic versioning**: `MAJOR.MINOR.PATCH` (e.g., `0.1.21`, `1.0.0`)
3. **Tag format**: Use `v` prefix (`v0.1.21`) for consistency
4. **Release notes**: Use the GitHub UI path for releases that need hand-written notes
5. **Test locally first**: Run `cargo build --release` and `./scripts/package-macos.sh` before releasing
6. **Check Actions**: Monitor the workflow to ensure all platforms build successfully

## Not yet automated

- macOS Intel builds (only `aarch64` is built)
- Windows code signing (the installer is unsigned)
