---
name: test
description: Run the Chatty test suite with cargo test. Use when the user wants to run tests, verify correctness, or check for regressions.
disable-model-invocation: true
allowed-tools: Bash, Read, Grep
---

# Run Chatty Tests

Run the project test suite and report results.

## Steps

1. Run `cargo test $ARGUMENTS` to execute tests. If no arguments are provided, run all tests.

2. If any tests fail:
   - Read the failing test source code to understand what's being tested
   - Identify the root cause of each failure
   - Suggest or apply fixes as appropriate

3. Report a summary: total tests, passed, failed, and skipped.
