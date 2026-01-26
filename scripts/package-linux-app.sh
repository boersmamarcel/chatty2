#!/bin/bash

set -e

APP_NAME="chatty"
BINARY_NAME="chatty"
VERSION="0.1.0"
IDENTIFIER="com.chatty"
DESCRIPTION="Chatty Application"
CATEGORIES="Development;Utility;"

RELEASE_DIR="target/release"
APP_DIR="${APP_NAME}-${VERSION}"
INSTALL_DIR="${APP_DIR}/usr"
BIN_DIR="${INSTALL_DIR}/bin"
SHARE_DIR="${INSTALL_DIR}/share"
APPLICATIONS_DIR="${SHARE_DIR}/applications"
ICONS_DIR="${SHARE_DIR}/icons/hicolor/256x256/apps"
DEBIAN_DIR="${APP_DIR}/DEBIAN"

echo "Creating Linux application package for ${APP_NAME}..."

# Check if binary exists
if [ ! -f "${RELEASE_DIR}/${BINARY_NAME}" ]; then
    echo "Error: Binary not found at ${RELEASE_DIR}/${BINARY_NAME}"
    echo "Please run 'cargo build --release' first"
    exit 1
fi

# Clean up old package
rm -rf "${APP_DIR}"
rm -f "${APP_NAME}-${VERSION}.deb"
rm -f "${APP_NAME}-${VERSION}.tar.gz"

# Create directory structure
mkdir -p "${BIN_DIR}"
mkdir -p "${APPLICATIONS_DIR}"
mkdir -p "${ICONS_DIR}"
mkdir -p "${DEBIAN_DIR}"

# Copy binary
cp "${RELEASE_DIR}/${BINARY_NAME}" "${BIN_DIR}/${BINARY_NAME}"
chmod +x "${BIN_DIR}/${BINARY_NAME}"

# Create .desktop file
cat > "${APPLICATIONS_DIR}/${IDENTIFIER}.desktop" << EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=${APP_NAME}
Comment=${DESCRIPTION}
Exec=${BINARY_NAME}
Icon=${IDENTIFIER}
Categories=${CATEGORIES}
Terminal=false
StartupNotify=true
EOF

# Create DEBIAN control file
cat > "${DEBIAN_DIR}/control" << EOF
Package: ${BINARY_NAME}
Version: ${VERSION}
Section: devel
Priority: optional
Architecture: amd64
Maintainer: Your Name <your.email@example.com>
Description: ${DESCRIPTION}
 A GPUI-based application with modern UI components.
EOF

# Create postinst script to update desktop database
cat > "${DEBIAN_DIR}/postinst" << 'EOF'
#!/bin/bash
if [ "$1" = "configure" ]; then
    update-desktop-database /usr/share/applications 2>/dev/null || true
fi
exit 0
EOF

chmod +x "${DEBIAN_DIR}/postinst"

# Create postrm script
cat > "${DEBIAN_DIR}/postrm" << 'EOF'
#!/bin/bash
if [ "$1" = "remove" ] || [ "$1" = "purge" ]; then
    update-desktop-database /usr/share/applications 2>/dev/null || true
fi
exit 0
EOF

chmod +x "${DEBIAN_DIR}/postrm"

echo "✓ Package structure created successfully"

# Build .deb package
echo ""
echo "Building .deb package..."
dpkg-deb --build "${APP_DIR}" "${APP_NAME}-${VERSION}.deb"

if [ -f "${APP_NAME}-${VERSION}.deb" ]; then
    echo "✓ Debian package created: ${APP_NAME}-${VERSION}.deb"
    echo ""
    echo "To install the package:"
    echo "  sudo dpkg -i ${APP_NAME}-${VERSION}.deb"
    echo "  sudo apt-get install -f  # if there are dependency issues"
    echo ""
fi

# Create portable tar.gz archive
echo "Creating portable tar.gz archive..."
PORTABLE_DIR="${APP_NAME}-${VERSION}-linux-x86_64"
rm -rf "${PORTABLE_DIR}"
mkdir -p "${PORTABLE_DIR}"

cp "${RELEASE_DIR}/${BINARY_NAME}" "${PORTABLE_DIR}/"
chmod +x "${PORTABLE_DIR}/${BINARY_NAME}"

# Create a simple README for the portable version
cat > "${PORTABLE_DIR}/README.txt" << EOF
${APP_NAME} v${VERSION}
==================

To run the application, execute:
  ./${BINARY_NAME}

You can move this directory anywhere and create a symbolic link:
  sudo ln -s \$(pwd)/${BINARY_NAME} /usr/local/bin/${BINARY_NAME}

Then you can run it from anywhere by typing:
  ${BINARY_NAME}
EOF

tar -czf "${APP_NAME}-${VERSION}-linux-x86_64.tar.gz" "${PORTABLE_DIR}"
rm -rf "${PORTABLE_DIR}"

if [ -f "${APP_NAME}-${VERSION}-linux-x86_64.tar.gz" ]; then
    echo "✓ Portable archive created: ${APP_NAME}-${VERSION}-linux-x86_64.tar.gz"
    echo ""
    echo "To use the portable version:"
    echo "  tar -xzf ${APP_NAME}-${VERSION}-linux-x86_64.tar.gz"
    echo "  cd ${APP_NAME}-${VERSION}-linux-x86_64"
    echo "  ./${BINARY_NAME}"
fi

echo ""
echo "Packaging complete!"
