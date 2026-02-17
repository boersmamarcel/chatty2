# Code Signing Setup Guide

This guide walks you through setting up code signing for Chatty on macOS, both locally and in GitHub Actions.

## Prerequisites

1. **Apple Developer Account** with a valid certificate
2. **Certificate Types**: You need one of these:
   - Developer ID Application (for distribution outside Mac App Store)
   - Apple Development (for local development only)
   - Mac App Distribution (for Mac App Store)

## Local Signing

### 1. Find Your Certificate Identity

```bash
security find-identity -v -p codesigning
```

This will output something like:
```
1) ABC123DEF456 "Developer ID Application: Your Name (TEAM_ID)"
2) XYZ789GHI012 "Apple Development: your@email.com (TEAM_ID)"
```

Copy the **full name in quotes** (e.g., "Developer ID Application: Your Name (TEAM_ID)")

### 2. Build and Sign Locally

```bash
# Export your certificate identity
export SIGNING_IDENTITY="Developer ID Application: Your Name (TEAM_ID)"

# Build release
cargo build --release

# Package and sign
./scripts/package-macos.sh
```

### 3. Optional: Enable Notarization

Notarization is required for Gatekeeper on macOS 10.15+. To enable:

```bash
# Store your app-specific password in keychain
xcrun notarytool store-credentials "AC_PASSWORD" \
  --apple-id "your@email.com" \
  --team-id "YOUR_TEAM_ID" \
  --password "xxxx-xxxx-xxxx-xxxx"

# Set environment variables
export SIGNING_IDENTITY="Developer ID Application: Your Name (TEAM_ID)"
export NOTARIZE_APPLE_ID="your@email.com"
export NOTARIZE_TEAM_ID="YOUR_TEAM_ID"

# Package will automatically notarize
./scripts/package-macos.sh
```

**Generate App-Specific Password:**
1. Go to https://appleid.apple.com
2. Sign In → Security → App-Specific Passwords
3. Generate new password
4. Use it in the `notarytool store-credentials` command above

## GitHub Actions Signing

### 1. Export Your Certificate

```bash
# Export certificate from Keychain to P12 file
security find-identity -v -p codesigning  # Find your certificate
# Note the hash (e.g., ABC123DEF456)

security export -k ~/Library/Keychains/login.keychain-db \
  -t identities \
  -f pkcs12 \
  -P "STRONG_PASSWORD_HERE" \
  -o certificate.p12
```

**IMPORTANT:** Use a strong password for the P12 file!

### 2. Convert Certificate to Base64

```bash
base64 -i certificate.p12 | pbcopy
```

This copies the base64-encoded certificate to your clipboard.

### 3. Add GitHub Secrets

Go to your GitHub repository → Settings → Secrets and variables → Actions → New repository secret

Add these secrets:

| Secret Name | Value | Description |
|-------------|-------|-------------|
| `MACOS_CERTIFICATE` | (paste from clipboard) | Base64-encoded P12 certificate |
| `MACOS_CERTIFICATE_PASSWORD` | (your P12 password) | Password used to export P12 |
| `KEYCHAIN_PASSWORD` | (generate random) | Temporary keychain password (use any strong random string) |
| `MACOS_SIGNING_IDENTITY` | `Developer ID Application: Your Name (TEAM_ID)` | Full certificate name from step 1 |

**Optional (for notarization):**

| Secret Name | Value | Description |
|-------------|-------|-------------|
| `NOTARIZE_APPLE_ID` | your@email.com | Your Apple ID email |
| `NOTARIZE_TEAM_ID` | ABC123XYZ | Your Team ID (10 character code) |

**Note:** Notarization in GitHub Actions uses `NOTARIZE_PASSWORD` env var directly since runners are ephemeral and can't use a local keychain profile. Add this additional secret:

| Secret Name | Value | Description |
|-------------|-------|-------------|
| `NOTARIZE_PASSWORD` | xxxx-xxxx-xxxx-xxxx | App-specific password from appleid.apple.com |

The script automatically detects whether to use the keychain profile (local) or the password directly (CI) based on whether `NOTARIZE_PASSWORD` is set.

### 4. Test Your Workflow

Push a tag to trigger the release workflow:

```bash
git tag v0.1.21
git push origin v0.1.21
```

The workflow will:
1. Import your certificate into a temporary keychain
2. Build the app
3. Sign with your Developer ID
4. (Optional) Notarize with Apple
5. Create DMG and upload to GitHub Release

## Verification

### Verify Local Signing

```bash
# Check signature
codesign --verify --verbose chatty.app

# Check Gatekeeper assessment
spctl --assess --verbose chatty.app
```

### Verify Notarization

```bash
# Check if notarization ticket is stapled
stapler validate chatty-macos-aarch64.dmg
```

## Troubleshooting

### "No identity found" Error

- Make sure the certificate is in your login keychain
- Run `security find-identity -v -p codesigning` to verify
- Ensure the certificate is valid and not expired

### Notarization Fails

- Verify your Apple ID and Team ID are correct
- Ensure your app-specific password is valid
- Check that hardened runtime is enabled (it is by default in the script)
- View detailed logs: `xcrun notarytool log <submission-id>`

### GitHub Actions Certificate Import Fails

- Verify the base64 encoding is correct (no extra newlines/spaces)
- Ensure the P12 password matches the secret
- Check that the certificate hasn't expired

## Clean Up

After exporting for GitHub:

```bash
# Remove the exported P12 file
rm certificate.p12

# Clear clipboard (if you're paranoid)
pbcopy < /dev/null
```

## Security Best Practices

1. **Never commit certificates** to your repository
2. Use **app-specific passwords**, not your main Apple ID password
3. Rotate passwords periodically
4. Use strong passwords for P12 files
5. Delete exported P12 files after uploading to GitHub
6. Use 2FA on your Apple Developer account
