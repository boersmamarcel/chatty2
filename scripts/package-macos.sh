#!/bin/bash

set -e

# Package name
APP_NAME="chatty"
IDENTIFIER="com.chatty"

# Extract version from Cargo.toml (single source of truth)
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
if [ -z "$VERSION" ]; then
    echo "Error: Could not extract version from Cargo.toml"
    exit 1
fi

RELEASE_DIR="target/release"
APP_BUNDLE="${APP_NAME}.app"
CONTENTS_DIR="${APP_BUNDLE}/Contents"
MACOS_DIR="${CONTENTS_DIR}/MacOS"
RESOURCES_DIR="${CONTENTS_DIR}/Resources"

echo "Creating macOS application bundle for ${APP_NAME} v${VERSION}..."

# Check if binary exists
if [ ! -f "${RELEASE_DIR}/${APP_NAME}" ]; then
    echo "Error: Binary not found at ${RELEASE_DIR}/${APP_NAME}"
    echo "Please run 'cargo build --release' first"
    exit 1
fi

# Clean up any existing bundle
rm -rf "${APP_BUNDLE}"

# Create bundle structure
mkdir -p "${MACOS_DIR}"
mkdir -p "${RESOURCES_DIR}"

# Copy binary
cp "${RELEASE_DIR}/${APP_NAME}" "${MACOS_DIR}/${APP_NAME}"
chmod +x "${MACOS_DIR}/${APP_NAME}"

# Copy icon if available
if [ -f "assets/app_icon/icon.icns" ]; then
    cp "assets/app_icon/icon.icns" "${RESOURCES_DIR}/"
fi

# Copy themes if available
if [ -d "themes" ]; then
    mkdir -p "${RESOURCES_DIR}/themes"
    cp themes/*.json "${RESOURCES_DIR}/themes/" 2>/dev/null || true
fi

# Create Info.plist
cat > "${CONTENTS_DIR}/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${IDENTIFIER}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleIconFile</key>
    <string>icon</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.13</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
EOF

echo "macOS application bundle created successfully: ${APP_BUNDLE}"

# Code signing configuration
# Set SIGNING_IDENTITY to your certificate name or use "-" for ad-hoc signing
# To find your certificate: security find-identity -v -p codesigning
SIGNING_IDENTITY="${SIGNING_IDENTITY:--}"

if [ "$SIGNING_IDENTITY" = "-" ]; then
    echo "Applying ad-hoc code signature..."
    codesign -s - --force --deep "${APP_BUNDLE}"
else
    echo "Applying code signature with identity: ${SIGNING_IDENTITY}"
    # Sign with hardened runtime and timestamp for notarization compatibility
    codesign --sign "${SIGNING_IDENTITY}" \
        --force \
        --options runtime \
        --timestamp \
        --deep \
        "${APP_BUNDLE}"

    # Verify signature
    codesign --verify --verbose "${APP_BUNDLE}"
    # Note: spctl will reject unnotarized apps, so we allow it to fail here
    spctl --assess --verbose "${APP_BUNDLE}" || echo "⚠️  App not yet notarized (will be notarized below)"
fi

# Create DMG for distribution
# Use simplified naming convention for auto-updater: chatty-macos-{arch}.dmg
# Map arm64 -> aarch64 to match Rust's arch convention
ARCH=$(uname -m)
if [ "$ARCH" = "arm64" ]; then
    ARCH="aarch64"
fi
DMG_NAME="${APP_NAME}-macos-${ARCH}.dmg"
echo "Creating DMG: ${DMG_NAME}..."
hdiutil create -volname "${APP_NAME}" -srcfolder "${APP_BUNDLE}" -ov -format UDZO "${DMG_NAME}" 2>/dev/null || {
    echo "Note: DMG creation skipped (hdiutil not available or failed)"
    echo "You can create a DMG manually with:"
    echo "  hdiutil create -volname ${APP_NAME} -srcfolder ${APP_BUNDLE} -ov -format UDZO ${DMG_NAME}"
}

# Optional: Notarization (requires NOTARIZE_APPLE_ID and NOTARIZE_TEAM_ID env vars)
if [ "$SIGNING_IDENTITY" != "-" ] && [ -n "$NOTARIZE_APPLE_ID" ] && [ -n "$NOTARIZE_TEAM_ID" ]; then
    echo "Submitting for notarization..."
    # Sign the DMG as well
    codesign --sign "${SIGNING_IDENTITY}" --timestamp "${DMG_NAME}"

    # Submit for notarization
    # Note: Requires app-specific password stored in keychain
    # Create with: xcrun notarytool store-credentials "AC_PASSWORD" --apple-id "your@email.com" --team-id "TEAM_ID"
    xcrun notarytool submit "${DMG_NAME}" \
        --keychain-profile "AC_PASSWORD" \
        --wait

    # Staple the notarization ticket
    xcrun stapler staple "${DMG_NAME}"
    echo "✅ Notarization complete"
else
    if [ "$SIGNING_IDENTITY" != "-" ]; then
        echo "ℹ️  Notarization skipped (set NOTARIZE_APPLE_ID and NOTARIZE_TEAM_ID to enable)"
    fi
fi
