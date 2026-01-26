# Package Chatty for Windows
# Run with: powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1

$ErrorActionPreference = "Stop"

# Package name and version from Cargo.toml
$APP_NAME = "chatty"
$VERSION = "0.1.0"

$RELEASE_DIR = "target\release"
$PACKAGE_DIR = "${APP_NAME}-${VERSION}-windows-x86_64"
$BINARY = "${APP_NAME}.exe"

Write-Host "Creating Windows package for ${APP_NAME} v${VERSION}..."

# Check if binary exists
if (-not (Test-Path "${RELEASE_DIR}\${BINARY}")) {
    Write-Host "Error: Binary not found at ${RELEASE_DIR}\${BINARY}" -ForegroundColor Red
    Write-Host "Please run 'cargo build --release' first"
    exit 1
}

# Clean up any existing package directory
if (Test-Path $PACKAGE_DIR) {
    Remove-Item -Recurse -Force $PACKAGE_DIR
}
if (Test-Path "${PACKAGE_DIR}.zip") {
    Remove-Item -Force "${PACKAGE_DIR}.zip"
}

# Create package structure
New-Item -ItemType Directory -Path $PACKAGE_DIR | Out-Null

# Copy binary
Copy-Item "${RELEASE_DIR}\${BINARY}" "${PACKAGE_DIR}\"

# Create a simple README
$README_CONTENT = @"
${APP_NAME} v${VERSION}

To run the application:
  Double-click ${BINARY} or run it from the command line.

For more information, visit: https://github.com/boersmamarcel/chatty2
"@
Set-Content -Path "${PACKAGE_DIR}\README.txt" -Value $README_CONTENT

# Create ZIP archive
Compress-Archive -Path $PACKAGE_DIR -DestinationPath "${PACKAGE_DIR}.zip"

# Clean up directory
Remove-Item -Recurse -Force $PACKAGE_DIR

Write-Host "Windows package created successfully: ${PACKAGE_DIR}.zip" -ForegroundColor Green
