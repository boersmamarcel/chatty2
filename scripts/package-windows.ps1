# Package Chatty for Windows
# Run with: powershell -ExecutionPolicy Bypass -File scripts/package-windows.ps1

$ErrorActionPreference = "Stop"

# Package name
$APP_NAME = "chatty"

# Extract version from Cargo.toml (single source of truth)
$VERSION = (Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
if (-not $VERSION) {
    Write-Host "Error: Could not extract version from Cargo.toml" -ForegroundColor Red
    exit 1
}

$RELEASE_DIR = "target\release"
# Use simplified naming convention for auto-updater: chatty-windows-{arch}.exe
$ARCH = "x86_64"  # Windows builds are x86_64
$PACKAGE_DIR = "${APP_NAME}-windows-${ARCH}"
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
