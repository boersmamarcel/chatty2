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

# Copy binary
cp "${RELEASE_DIR}/${APP_NAME}" "${PACKAGE_DIR}/"
chmod +x "${PACKAGE_DIR}/${APP_NAME}"

# Create a simple README
cat > "${PACKAGE_DIR}/README.txt" << EOF
${APP_NAME} v${VERSION}

To run the application:
  ./${APP_NAME}

For more information, visit: https://github.com/boersmamarcel/chatty2
EOF

# Create tarball
tar -czvf "${PACKAGE_DIR}.tar.gz" "${PACKAGE_DIR}"

# Clean up directory
rm -rf "${PACKAGE_DIR}"

echo "Linux package created successfully: ${PACKAGE_DIR}.tar.gz"
