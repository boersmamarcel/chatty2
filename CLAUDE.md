# Chatty

A desktop chat application built with Rust and GPUI.

## Tech Stack

- **UI Framework**: [GPUI](https://crates.io/crates/gpui) - Zed's GPU-accelerated UI framework
- **Components**: gpui-component for UI components
- **LLM Integration**: [rig-core](https://crates.io/crates/rig-core) for LLM operations
- **Async Runtime**: Tokio
- **Serialization**: serde/serde_json for persistence

## Project Structure

```
src/
├── main.rs              # Application entry point, initialization, theme handling
├── chatty/              # Main chat application module
└── settings/            # Settings system
    ├── controllers/     # Settings window controllers
    ├── models/          # Data models (providers, models, general settings)
    ├── providers/       # Provider implementations (e.g., Ollama)
    ├── repositories/    # Persistence layer (JSON file storage)
    ├── utils/           # Utilities (theme helpers)
    └── views/           # Settings UI views
```

## Build Commands

```bash
# Build debug
cargo build

# Build release
cargo build --release

# Run tests
cargo test

# Check formatting
cargo fmt --check

# Run clippy lints
cargo clippy -- -D warnings
```

## Packaging

Scripts are in `scripts/`:

```bash
# Package for macOS (creates .app bundle and .dmg)
./scripts/package-macos.sh

# Package for Linux (creates .tar.gz)
./scripts/package-linux.sh
```

## Linux Dependencies

For building on Linux, install these system packages:

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

## Architecture Notes

- **Tokio Runtime**: The app uses Tokio for all async operations. The runtime is entered at startup and maintained throughout the application lifecycle.
- **Global State**: Uses GPUI's global state system (`cx.set_global`, `cx.global`) for app-wide state like providers, models, and settings.
- **Async Loading**: Providers, models, and settings are loaded asynchronously to avoid blocking the UI during startup.
- **Theme System**: Themes are loaded from `./themes` directory. User preferences (theme name + dark mode) are persisted to JSON.

## CI/CD

- **CI**: Runs on pull requests to `main` - tests, formatting check, and clippy
- **Release**: Runs on push to `main` - builds for Linux x86_64, macOS Intel, and macOS ARM, then creates GitHub releases
