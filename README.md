# Chatty &emsp; [![Build Status][build-badge]][build-url] [![License][license-badge]][license-url] [![Made with Rust][rust-badge]][rust-url] [![Maintenance][maintenance-badge]][repo-url]

[build-badge]: https://github.com/boersmamarcel/chatty2/actions/workflows/ci.yml/badge.svg?branch=main
[build-url]: https://github.com/boersmamarcel/chatty2/actions/workflows/ci.yml?query=branch%3Amain
[license-badge]: https://img.shields.io/badge/License-MIT-yellow.svg
[license-url]: #license
[rust-badge]: https://img.shields.io/badge/Made%20with-Rust-1f425f.svg?logo=rust
[rust-url]: https://www.rust-lang.org/
[maintenance-badge]: https://img.shields.io/badge/maintenance-actively--developed-brightgreen.svg
[repo-url]: https://github.com/boersmamarcel/chatty2

A desktop chat application built with Rust and GPUI, supporting multiple LLM providers.

## Features

- **Multi-Provider Support**: Anthropic, OpenAI, Mistral, Gemini, Groq, and Ollama
- **Modern UI**: Built with [GPUI](https://crates.io/crates/gpui) - Zed's GPU-accelerated UI framework
- **MCP Integration**: Model Context Protocol server support (Obsidian, Filesystem, GitHub)
- **Local Inference**: Offline support via Ollama
- **Rich Content**: Image and PDF attachment support
- **Math Rendering**: LaTeX math expressions via Typst
- **Code Highlighting**: Syntax highlighting for code blocks
- **Auto-Updates**: Built-in update mechanism

## Tech Stack

- **UI Framework**: [GPUI](https://crates.io/crates/gpui) - GPU-accelerated UI framework
- **Components**: gpui-component for UI components
- **LLM Integration**: [rig-core](https://crates.io/crates/rig-core) for LLM operations
- **Async Runtime**: Tokio
- **Serialization**: serde/serde_json for persistence

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

## Packaging

Scripts are available in `scripts/`:

```bash
# Package for macOS (creates .app bundle and .dmg)
./scripts/package-macos.sh

# Package for Linux (creates .tar.gz)
./scripts/package-linux.sh
```

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

## Architecture

- **Tokio Runtime**: The app uses Tokio for all async operations
- **Global State**: Uses GPUI's global state system for app-wide state
- **Async Loading**: Providers, models, and settings are loaded asynchronously
- **Theme System**: Themes loaded from `./themes` directory with user preferences persisted
- **Math Cache**: LaTeX expressions compiled to SVG using Typst, cached in platform-specific directories

## CI/CD

- **CI**: Runs on pull requests to `main` - tests, formatting check, and clippy
- **Release**: Runs on push to `main` - builds for Linux x86_64, macOS Intel, and macOS ARM

## License

Licensed under the MIT License. See [LICENSE](LICENSE) for details.

Copyright (c) 2026 Marcel Boersma
