# Build & package

**When to read this:** Produce release artifacts or run CI locally.

## Local CI

```bash
make setup        # Linux deps + wasm32-wasip2 (once)
make wasm-modules # echo-agent WASM for tests
make ci           # matches the Rust path of GitHub Actions
```

GitHub skips that compile path when a PR only changes docs. Use `make docs`
and `make docs-check-links` for documentation-only work.

## Platform packages

| Platform | Script |
|----------|--------|
| macOS | `./scripts/package-macos.sh` |
| Linux | `./scripts/package-linux.sh` |

See [RELEASE_PROCESS.md](../architecture/RELEASE_PROCESS.md) for version bumps and GitHub Releases.

## Docs site

```bash
make docs-gen
make docs
make docs-serve
```
