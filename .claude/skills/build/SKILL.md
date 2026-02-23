---
name: build
description: Build the Chatty project with cargo, run clippy lints, and check formatting. Use when the user wants to compile, lint, or verify code quality.
disable-model-invocation: true
allowed-tools: Bash
---

# Build Chatty

Run the full build pipeline for the Chatty project. Execute the following steps in order, stopping if any step fails:

## Steps

1. **Format check**: Run `cargo fmt --check` to verify formatting. If it fails, run `cargo fmt` to auto-fix, then report what changed.

2. **Clippy lints**: Run `cargo clippy -- -D warnings` to check for lint issues. If there are warnings, fix them before proceeding.

3. **Build**: Run `cargo build` for a debug build. If `$ARGUMENTS` contains "release", run `cargo build --release` instead.

Report a summary of each step's result (pass/fail) and any issues found.
