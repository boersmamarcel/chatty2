---
name: package
description: Package the Chatty application for distribution on macOS or Linux. Use when building release binaries, app bundles, or distribution archives.
disable-model-invocation: true
allowed-tools: Bash, Read
argument-hint: [macos|linux]
---

# Package Chatty for Distribution

Package the Chatty application for the specified platform.

## Steps

1. **Determine target platform**: Use `$ARGUMENTS` if provided ("macos" or "linux"). If not specified, detect the current platform.

2. **Build release binary**: Run `cargo build --release`

3. **Run packaging script**:
   - **macOS**: Run `./scripts/package-macos.sh` (creates .app bundle and .dmg)
   - **Linux**: Run `./scripts/package-linux.sh` (creates .tar.gz)

4. **Verify output**: Check that the packaging artifact was created successfully and report its location and size.

## Notes

- macOS packaging requires Xcode command line tools
- Linux packaging requires the system dependencies listed in CLAUDE.md
- The release build uses optimizations and may take longer than debug builds
