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
ARCH=$(uname -m)
APPDIR="${APP_NAME}.AppDir"
APPIMAGE_NAME="${APP_NAME}-linux-${ARCH}.AppImage"

echo "Creating AppImage for ${APP_NAME} v${VERSION}..."

# Check if binary exists
if [ ! -f "${RELEASE_DIR}/${APP_NAME}" ]; then
    echo "Error: Binary not found at ${RELEASE_DIR}/${APP_NAME}"
    echo "Please run 'cargo build --release' first"
    exit 1
fi

# Check for appimagetool
if ! command -v appimagetool &> /dev/null; then
    echo "appimagetool not found. Downloading..."
    APPIMAGETOOL="appimagetool-${ARCH}.AppImage"
    if [ ! -f "${APPIMAGETOOL}" ]; then
        wget -q "https://github.com/AppImage/AppImageKit/releases/download/continuous/${APPIMAGETOOL}"
        chmod +x "${APPIMAGETOOL}"
    fi
    APPIMAGETOOL="./${APPIMAGETOOL}"
else
    APPIMAGETOOL="appimagetool"
fi

# Clean up any existing AppDir
rm -rf "${APPDIR}"
rm -f "${APPIMAGE_NAME}"

# Create AppDir structure
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/16x16/apps"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/32x32/apps"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/64x64/apps"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/128x128/apps"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/256x256/apps"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/512x512/apps"

# Copy binary
cp "${RELEASE_DIR}/${APP_NAME}" "${APPDIR}/usr/bin/"
chmod +x "${APPDIR}/usr/bin/${APP_NAME}"

# Copy icons
if [ -d "assets/app_icon" ]; then
    [ -f "assets/app_icon/ai-7.png" ] && cp "assets/app_icon/ai-7.png" "${APPDIR}/usr/share/icons/hicolor/16x16/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai-5.png" ] && cp "assets/app_icon/ai-5.png" "${APPDIR}/usr/share/icons/hicolor/32x32/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai-4.png" ] && cp "assets/app_icon/ai-4.png" "${APPDIR}/usr/share/icons/hicolor/64x64/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai-3.png" ] && cp "assets/app_icon/ai-3.png" "${APPDIR}/usr/share/icons/hicolor/128x128/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai-2.png" ] && cp "assets/app_icon/ai-2.png" "${APPDIR}/usr/share/icons/hicolor/256x256/apps/${APP_NAME}.png"
    [ -f "assets/app_icon/ai.png" ] && cp "assets/app_icon/ai.png" "${APPDIR}/usr/share/icons/hicolor/512x512/apps/${APP_NAME}.png"

    # Copy 256x256 as the main icon (standard for AppImage)
    [ -f "assets/app_icon/ai-2.png" ] && cp "assets/app_icon/ai-2.png" "${APPDIR}/${APP_NAME}.png"
fi

# Copy themes if available
if [ -d "themes" ]; then
    mkdir -p "${APPDIR}/usr/share/${APP_NAME}/themes"
    cp themes/*.json "${APPDIR}/usr/share/${APP_NAME}/themes/" 2>/dev/null || true
fi

# Create .desktop file
cat > "${APPDIR}/${APP_NAME}.desktop" << EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=Chatty
Comment=Desktop chat application
Exec=${APP_NAME}
Icon=${APP_NAME}
Terminal=false
Categories=Network;InstantMessaging;
StartupWMClass=chatty
EOF

# Create AppRun script with desktop integration
cat > "${APPDIR}/AppRun" << 'APPRUN_EOF'
#!/bin/bash
SELF=$(readlink -f "$0")
HERE=${SELF%/*}
APPIMAGE_PATH=$(readlink -f "${APPIMAGE:-$0}")

# Desktop integration function
integrate_desktop() {
    local DESKTOP_DIR="${HOME}/.local/share/applications"
    local ICON_DIR="${HOME}/.local/share/icons/hicolor"
    local DESKTOP_FILE="${DESKTOP_DIR}/chatty.desktop"

    # Check if already integrated
    if [ -f "$DESKTOP_FILE" ]; then
        # Update Exec path if AppImage moved
        if ! grep -q "Exec=${APPIMAGE_PATH}" "$DESKTOP_FILE" 2>/dev/null; then
            sed -i "s|^Exec=.*|Exec=${APPIMAGE_PATH}|" "$DESKTOP_FILE"
        fi
        return
    fi

    # Create directories
    mkdir -p "$DESKTOP_DIR"
    mkdir -p "$ICON_DIR/16x16/apps"
    mkdir -p "$ICON_DIR/32x32/apps"
    mkdir -p "$ICON_DIR/64x64/apps"
    mkdir -p "$ICON_DIR/128x128/apps"
    mkdir -p "$ICON_DIR/256x256/apps"
    mkdir -p "$ICON_DIR/512x512/apps"

    # Install icons
    [ -f "${HERE}/usr/share/icons/hicolor/16x16/apps/chatty.png" ] && cp "${HERE}/usr/share/icons/hicolor/16x16/apps/chatty.png" "$ICON_DIR/16x16/apps/"
    [ -f "${HERE}/usr/share/icons/hicolor/32x32/apps/chatty.png" ] && cp "${HERE}/usr/share/icons/hicolor/32x32/apps/chatty.png" "$ICON_DIR/32x32/apps/"
    [ -f "${HERE}/usr/share/icons/hicolor/64x64/apps/chatty.png" ] && cp "${HERE}/usr/share/icons/hicolor/64x64/apps/chatty.png" "$ICON_DIR/64x64/apps/"
    [ -f "${HERE}/usr/share/icons/hicolor/128x128/apps/chatty.png" ] && cp "${HERE}/usr/share/icons/hicolor/128x128/apps/chatty.png" "$ICON_DIR/128x128/apps/"
    [ -f "${HERE}/usr/share/icons/hicolor/256x256/apps/chatty.png" ] && cp "${HERE}/usr/share/icons/hicolor/256x256/apps/chatty.png" "$ICON_DIR/256x256/apps/"
    [ -f "${HERE}/usr/share/icons/hicolor/512x512/apps/chatty.png" ] && cp "${HERE}/usr/share/icons/hicolor/512x512/apps/chatty.png" "$ICON_DIR/512x512/apps/"

    # Update icon cache (silently)
    gtk-update-icon-cache -f -t "$ICON_DIR" 2>/dev/null || true

    # Create .desktop file
    cat > "$DESKTOP_FILE" << DESKTOP_EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=Chatty
Comment=Desktop chat application
Exec=${APPIMAGE_PATH}
Icon=chatty
Terminal=false
Categories=Network;InstantMessaging;
StartupWMClass=chatty
DESKTOP_EOF

    # Make desktop file executable (required by some DEs)
    chmod +x "$DESKTOP_FILE"
}

# Perform desktop integration in background
integrate_desktop &

# Set up environment for themes
export CHATTY_DATA_DIR="${HERE}/usr/share/chatty"

# Run the application
exec "${HERE}/usr/bin/chatty" "$@"
APPRUN_EOF
chmod +x "${APPDIR}/AppRun"

# Build the AppImage
ARCH="${ARCH}" "${APPIMAGETOOL}" "${APPDIR}" "${APPIMAGE_NAME}"

# Clean up AppDir
rm -rf "${APPDIR}"

echo ""
echo "AppImage created successfully: ${APPIMAGE_NAME}"
echo ""
echo "To run: ./${APPIMAGE_NAME}"
echo "Or make it executable and double-click in your file manager."
