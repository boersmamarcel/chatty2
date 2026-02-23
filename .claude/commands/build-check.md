Run the full local CI pipeline: formatting, linting, tests, and build.

Steps:

1. Run `cargo fmt --check` to verify formatting
2. Run `cargo clippy -- -D warnings` to check for lint warnings
3. Run `cargo test` to execute all tests
4. Run `cargo build` to verify compilation

Report each step's result. If any step fails, stop and report the failure with the specific error output so it can be fixed. Do not skip failing steps.
