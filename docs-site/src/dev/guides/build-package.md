# Build & package

**When to read this:** Produce release artifacts or run CI locally.

## Goal

The same compile, test and lint path GitHub runs, executed on your machine; and, when you need them, the platform packages the release workflow builds.

## Prerequisites

- [Build and run](../start/build-and-run.md) done once (`make setup`, `make wasm-modules`).
- About 16 GiB of `target/` for the full test build ([Build disk usage](../architecture/build-disk-usage.md)).

## Steps

### Local CI

```bash
make setup        # Linux deps + wasm32-wasip2 (once)
make wasm-modules # echo-agent WASM for tests
make ci           # matches the Rust path of GitHub Actions
```

`make ci` runs `cargo test --all-features -- --test-threads=1`, `cargo fmt --check`, `cargo clippy --all-features -- -D warnings`, and the reserved-symbol and rig-pin scripts. GitHub skips that compile path when a PR only changes docs; use `make docs` and `make docs-check` for documentation-only work. Every target is listed in [Make targets & CI workflows](../ci-reference.md); which tests to run for a smaller change is in [Test](./test.md).

### Platform packages

| Platform | Script |
|----------|--------|
| macOS | `./scripts/package-macos.sh` (`.app` bundle and `.dmg`) |
| Linux | `./scripts/package-linux.sh` (`.tar.gz`) |

Release builds write a separate `target/release/` tree; only build one locally when you are packaging.

### Cutting a release

Version bumps, changelog generation, tagging and the cross-platform release build are driven by labels on the PR and the Prepare Release workflow. See the [Release process](../architecture/RELEASE_PROCESS.md).

### Docs site

```bash
make docs-gen
make docs
make docs-serve
```

## Verify

`make ci` exits 0; a package script leaves its artifact in the location it prints.

## Common mistakes

| Mistake | Fix |
|---------|-----|
| Running `cargo test` without `--all-features` and calling it CI | Feature-gated tools never compiled; use `make test` |
| Adding a release label after merging | Labels are read from the merged PR; see [Release process](../architecture/RELEASE_PROCESS.md) |
