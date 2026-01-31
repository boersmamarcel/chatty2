#!/bin/bash

set -e

# Package name
APP_NAME="chatty"

# Extract version from Cargo.toml (single source of truth)
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
if [ -z "$VERSION" ]; then
    echo "Error: Could not extract version from Cargo.toml"
    exit 1
fi

RELEASE_DIR="target/release"
# Use simplified naming convention for auto-updater: chatty-linux-{arch}.tar.gz
ARCH=$(uname -m)
PACKAGE_DIR="${APP_NAME}-linux-${ARCH}"

echo "Creating Linux package for ${APP_NAME} v${VERSION}..."

# Check if binary exists
if [ ! -f "${RELEASE_DIR}/${APP_NAME}" ]; then
    echo "Error: Binary not found at ${RELEASE_DIR}/${APP_NAME}"
    echo "Please run 'cargo build --release' first"
    exit 1
fi

# Clean up any existing package directory
rm -rf "${PACKAGE_DIR}"
rm -f "${PACKAGE_DIR}.tar.gz"

# Create package structure
mkdir -p "${PACKAGE_DIR}"
mkdir -p "${PACKAGE_DIR}/icons/hicolor/16x16/apps"
mkdir -p "${PACKAGE_DIR}/icons/hicolor/32x32/apps"
mkdir -p "${PACKAGE_DIR}/icons/hicolor/64x64/apps"
mkdir -p "${PACKAGE_DIR}/icons/hicolor/128x128/apps"
mkdir -p "${PACKAGE_DIR}/icons/hicolor/256x256/apps"
mkdir -p "${PACKAGE_DIR}/icons/hicolor/512x512/apps"

# Copy binary
cp "${RELEASE_DIR}/${APP_NAME}" "${PACKAGE_DIR}/"
chmod +x "${PACKAGE_DIR}/${APP_NAME}"

# Copy icons if available
if [ -d "assets/app_icon" ]; then
    [ -f "assets/app_icon/ai-7.png" ] && cp "assets/app_icon/ai-7.png" "${PACKAGE_DIR}/icons/hicolor/16x16/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai-5.png" ] && cp "assets/app_icon/ai-5.png" "${PACKAGE_DIR}/icons/hicolor/32x32/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai-4.png" ] && cp "assets/app_icon/ai-4.png" "${PACKAGE_DIR}/icons/hicolor/64x64/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai-3.png" ] && cp "assets/app_icon/ai-3.png" "${PACKAGE_DIR}/icons/hicolor/128x128/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai-2.png" ] && cp "assets/app_icon/ai-2.png" "${PACKAGE_DIR}/icons/hicolor/256x256/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai.png" ] && cp "assets/app_icon/ai.png" "${PACKAGE_DIR}/icons/hicolor/512x512/apps/${APP_NAME}.png"
fi

# Copy themes if available
if [ -d "themes" ]; then
    mkdir -p "${PACKAGE_DIR}/themes"
    cp themes/*.json "${PACKAGE_DIR}/themes/" 2>/dev/null || true
fi

# Create .desktop file
cat > "${PACKAGE_DIR}/${APP_NAME}.desktop" << EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=Chatty
Comment=Desktop chat application
Exec=${APP_NAME}
Icon=${APP_NAME}
Terminal=false
Categories=Network;InstantMessaging;
EOF

# Create a simple README
cat > "${PACKAGE_DIR}/README.txt" << EOF
${APP_NAME} v${VERSION}

To run the application:
  ./${APP_NAME}

To install icons system-wide (optional):
  sudo cp -r icons/hicolor/* /usr/share/icons/hicolor/
  sudo gtk-update-icon-cache /usr/share/icons/hicolor/

To install .desktop file (optional):
  cp ${APP_NAME}.desktop ~/.local/share/applications/

For more information, visit: https://github.com/boersmamarcel/chatty2
EOF

# Create tarball
tar -czvf "${PACKAGE_DIR}.tar.gz" "${PACKAGE_DIR}"

# Clean up directory
rm -rf "${PACKAGE_DIR}"

echo "Linux package created successfully: ${PACKAGE_DIR}.tar.gz"
