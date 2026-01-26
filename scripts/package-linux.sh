#!/bin/bash

set -e

# Package name and version from Cargo.toml
APP_NAME="chatty"
VERSION="0.1.0"

RELEASE_DIR="target/release"
PACKAGE_DIR="${APP_NAME}-${VERSION}-linux-x86_64"

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
