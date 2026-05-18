#!/usr/bin/env bash
# scripts/setup-linux.sh
#
# Install Linux system packages required to build chatty-gpui and
# chatty-tui on Debian/Ubuntu derivatives, and add the wasm32-wasip2 Rust
# target needed by the WASM agent modules and integration tests.
#
# Mirrors the package list in .github/workflows/ci.yml. When CI changes,
# update this script too.
#
# Usage:
#   bash scripts/setup-linux.sh

set -euo pipefail

if ! command -v apt-get >/dev/null 2>&1; then
  echo "This script targets Debian/Ubuntu (apt-get not found)."
  echo "On other distros, install the equivalents of the package list below"
  echo "and run: rustup target add wasm32-wasip2"
  exit 1
fi

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

echo "==> Installing Linux system packages required by chatty-gpui"
$SUDO apt-get update
$SUDO apt-get install -y \
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

if command -v rustup >/dev/null 2>&1; then
  echo "==> Adding wasm32-wasip2 Rust target"
  rustup target add wasm32-wasip2
else
  echo "Note: rustup not found; skipping wasm32-wasip2 target install."
  echo "Install rustup from https://rustup.rs, then run:"
  echo "  rustup target add wasm32-wasip2"
fi

echo "==> Done. Next steps:"
echo "  make wasm-modules    # build the echo-agent WASM (needed by tests)"
echo "  make test            # full CI-equivalent test suite"
