# Release Process

## Automated Release (CI/CD)

1. Update version in `Cargo.toml`
2. Commit: `git commit -am "Bump version to X.Y.Z"`
3. Tag: `git tag vX.Y.Z`
4. Push: `git push origin main --tags`
5. GitHub Actions will automatically build and create the release

## Local Builds

### Prerequisites

- Rust toolchain (`rustup` with stable channel)
- Platform-specific tools (see below)

### Windows

**Requirements:**
- [Inno Setup 6](https://jrsoftware.org/isdl.php) (or install via `choco install innosetup -y`)

**Build:**
```powershell
# Build the release binary
cargo build --release

# Create the installer (replace 0.1.5 with your version)
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" /DMyAppVersion=0.1.5 scripts/installer-windows.iss
```

**Output:** `scripts/chatty-windows-x86_64.exe`

### macOS

**Requirements:**
- Xcode command line tools (`xcode-select --install`)
- `create-dmg` (install via `brew install create-dmg`)

**Build:**
```bash
# Build the release binary
cargo build --release

# Create the .app bundle and .dmg
./scripts/package-macos.sh
```

**Output:** `chatty-macos-aarch64.dmg` (or `chatty-macos-x86_64.dmg` on Intel)

### Linux

**Requirements:**
```bash
sudo apt-get install -y \
  libxkbcommon-dev \
  libxkbcommon-x11-dev \
  libwayland-dev \
  libvulkan-dev \
  libx11-dev \
  libxcb1-dev \
  libxcb-render0-dev \
  libxcb-shape0-dev \
  libxcb-xfixes0-dev \
  libxcursor-dev \
  libxrandr-dev \
  libxi-dev \
  libgl1-mesa-dev \
  libfontconfig1-dev \
  libasound2-dev \
  libssl-dev \
  pkg-config
```

**Build:**
```bash
# Build the release binary
cargo build --release

# Create the AppImage
./scripts/package-linux-appimage.sh
```

**Output:** `chatty-linux-x86_64.AppImage`

## Output File Naming Convention

The auto-updater expects specific file names:
- Windows: `chatty-windows-x86_64.exe`
- macOS ARM: `chatty-macos-aarch64.dmg`
- macOS Intel: `chatty-macos-x86_64.dmg`
- Linux: `chatty-linux-x86_64.AppImage`
