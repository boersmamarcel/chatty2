---
name: build-and-check
description: Runs the full build pipeline for Chatty including compilation, tests, formatting check, and clippy lints. Use when the user asks to build, test, check, or validate the project, or before committing changes.
allowed-tools: Bash, Read, Grep, Glob
---

# Build and Check

Runs the complete Chatty build and validation pipeline. Execute each step sequentially and report results.

## Steps

1. **Build** the project:
   ```bash
   cargo build 2>&1
   ```

2. **Run tests** with all features enabled:
   ```bash
   cargo test --all-features 2>&1
   ```

3. **Check formatting**:
   ```bash
   cargo fmt --check 2>&1
   ```

4. **Run clippy lints** (warnings as errors):
   ```bash
   cargo clippy -- -D warnings 2>&1
   ```

## Reporting

After running all steps, provide a summary:

- For each step, report PASS or FAIL
- If any step fails, show the relevant error output
- For clippy/fmt failures, identify the affected files and suggest fixes
- For test failures, show the failing test names and error messages

## Fixing Issues

If the user asks to fix issues found:

- **Formatting**: Run `cargo fmt` to auto-fix
- **Clippy**: Apply the suggested fixes, being careful not to change behavior
- **Test failures**: Investigate the root cause before making changes
- **Build errors**: Check for missing imports, type mismatches, or dependency issues

After fixing, re-run the failing step to confirm the fix.

## Notes

- On Linux, the build requires system packages listed in CLAUDE.md under "Linux Dependencies"
- The project uses Rust edition 2024 with Tokio async runtime
- Tests may require the Tokio runtime context
