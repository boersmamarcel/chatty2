---
audience: [contributor]
source_files:
  - crates/chatty-gpui/src/auto_updater/signature.rs
  - crates/chatty-gpui/src/auto_updater/release_keys.txt
  - crates/chatty-gpui/src/auto_updater/platform.rs
  - .github/workflows/release.yml
related:
  - RELEASE_PROCESS.md
---

# Release signing and update authenticity

**When to read this:** you are setting up or rotating the release key, a release
failed at `sign-checksums`, or a user's update was refused.

## Security note (SEC-10, AGE-817)

`checksums.txt` proves a download is intact, but it comes from the same GitHub
release as the binary. Anyone who can write the release (a leaked token, a
compromised workflow) can replace both. Two checks close that gap:

- **Signed checksums, every platform.** `release.yml` signs `checksums.txt` with an
  Ed25519 release key in [minisign](https://jedisct1.github.io/minisign/) format and
  uploads `checksums.txt.sig`. The updater downloads both and verifies the signature
  against the public keys compiled into it (`auto_updater/release_keys.txt`)
  **before** it trusts any hash. A missing, malformed or foreign signature refuses
  the update; there is no unsigned fallback.
- **macOS Team ID before the swap.** After the DMG is mounted and before anything is
  copied out of it, the install helper requires: the DMG passes Gatekeeper as
  `Notarized Developer ID` (its stapled ticket), the app inside passes
  `codesign --verify --deep --strict`, and its `TeamIdentifier` equals the Team ID
  compiled into the running app (`CHATTY_MACOS_TEAM_ID`, set by `release.yml` from the
  `NOTARIZE_TEAM_ID` secret). On refusal the helper leaves the installed app untouched
  and relaunches it; the reason is in `~/Library/Logs/chatty_update.log`.

Not covered: Windows has no code-signing certificate and releases carry no build
provenance (decided in AGE-736). An attacker holding the release **private key** can
still ship a signed update; the key therefore lives only in a protected environment
that a single job can read.

### The one unpinned state

While `release_keys.txt` lists no key, the updater skips the signature check and logs
`No release signing key is pinned in this build`. That is the state before the
runbook below has run. A build without a compiled Team ID (local builds, or releases
with `MACOS_SIGNING_ENABLED` off) skips the Team-ID check the same way: its own
releases carry no Team ID.

## Where things live

| Thing | Where |
|---|---|
| Private key | `RELEASE_SIGNING_KEY` secret (base64 of the minisign secret-key file) in the `release-signing` GitHub environment; offline copy in your password manager |
| Public keys | `crates/chatty-gpui/src/auto_updater/release_keys.txt`, one `RW...` line per key |
| Switch | repo variable `RELEASE_SIGNING_ENABLED` (`true` = sign every release) |
| Signing job | `sign-checksums` in `.github/workflows/release.yml` |
| Key inventory | vault `fabric-security` §7 |

`release.yml` enforces the pairing both ways: with the switch on, a missing secret or
a signature that no pinned key accepts fails the release; with the switch off, a
pinned key fails the release. Either way no unsigned release reaches pinned clients.

## Runbook: first key

Run on your own machine with `minisign` installed (`brew install minisign` or
`apt install minisign`). None of these commands prints the private key.

1. **Generate the key** in a private scratch directory, without a password (the CI
   job cannot type one; the protected environment is the access control):

   ```bash
   umask 077 && mkdir -p ~/chatty-release-key && cd ~/chatty-release-key
   minisign -G -W -p release.pub -s release.key
   ```

2. **Create the environment and set the secret.** Restrict the environment to the
   refs releases run from (`main` for `prepare-release.yml`, `v*` tags for the
   published-release and dispatch paths). Do not add required reviewers: releases are
   cut automatically and would wait forever.

   ```bash
   gh api -X PUT repos/boersmamarcel/chatty2/environments/release-signing \
     -F 'deployment_branch_policy[protected_branches]=false' \
     -F 'deployment_branch_policy[custom_branch_policies]=true'
   gh api -X POST repos/boersmamarcel/chatty2/environments/release-signing/deployment-branch-policies \
     -f name=main -f type=branch
   gh api -X POST repos/boersmamarcel/chatty2/environments/release-signing/deployment-branch-policies \
     -f name='v*' -f type=tag
   base64 < release.key | tr -d '\n' \
     | gh secret set RELEASE_SIGNING_KEY --env release-signing --repo boersmamarcel/chatty2
   ```

3. **Flip the switch.** From now on every release carries `checksums.txt.sig`
   (current installs ignore it; the signing job warns that no key is pinned yet).

   ```bash
   gh variable set RELEASE_SIGNING_ENABLED --body true --repo boersmamarcel/chatty2
   ```

4. **Pin the public key.** Add the second line of `release.pub` (the one starting
   with `RW`) to `release_keys.txt` in a PR. Merge it after step 3, never before: a
   build that pins a key must never be followed by an unsigned release.

5. **Release.** The signing job now also checks the signature against the pinned key.
   Installs that update to this release enforce the signature from then on.

6. **Store and clean up.** Put `release.key` and `release.pub` in your password
   manager, then `rm -P` (macOS) or `shred -u` (Linux) both files. Ask the coordinator
   to add the key ID (`minisign -V` prints it) to the vault key inventory.

## Rotation (two-key window, SEC-3)

1. Generate the new key (step 1) and add its public line to `release_keys.txt`
   next to the old one. Release: installs now trust both.
2. Replace the `RELEASE_SIGNING_KEY` secret with the new key (step 2, last command).
   Release: signed by the new key, accepted by every install that took step 1's
   release.
3. Remove the old line from `release_keys.txt`. Release.

On a suspected private-key leak, skip the window: pin only the new key, set the new
secret, release at once. Installs still on old builds trust the leaked key until they
update, so revoke the leaked secret immediately.

## When a release fails at signing

- `RELEASE_SIGNING_ENABLED is true but the RELEASE_SIGNING_KEY secret is not set`:
  step 2 did not run, or the secret was set at repo level instead of in the
  `release-signing` environment.
- `does not verify against any key in release_keys.txt`: the secret and the pinned
  public key are from different pairs. Fix whichever is wrong; never remove the pin
  to get a release out.
- `release_keys.txt pins a release key but RELEASE_SIGNING_ENABLED is off`: turn the
  switch back on.
