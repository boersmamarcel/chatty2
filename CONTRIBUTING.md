# Contributing to Chatty

Thank you for contributing to [chatty2](https://github.com/boersmamarcel/chatty2).

## Documentation

| Resource | Location |
|----------|----------|
| **Developer docs (site)** | `make docs-serve` → [GitHub Pages](https://boersmamarcel.github.io/chatty2/) |
| **Agent quick-start** | [`AGENTS.md`](AGENTS.md) |
| **Coding patterns** | [`CLAUDE.md`](CLAUDE.md) |
| **Architecture index** | [`docs/INDEX.md`](docs/INDEX.md) |
| **Stale-doc policy** | [`docs/stale-doc-policy.md`](docs/stale-doc-policy.md) |
| **Component diagrams** | [`docs/component-map.md`](docs/component-map.md) |
| **Marketing / end users** | [github.com/boersmamarcel/chatty](https://github.com/boersmamarcel/chatty) |

## Development workflow

```bash
make setup        # once on Linux
make wasm-modules # before full test suite
make ci           # same compile/test/lint path GitHub runs for Rust PRs
```

Docs-only pull requests do not compile the workspace on GitHub. Run `make docs`
and `make docs-check-links` for documentation changes.

If a change alters a fact a page claims, update that page in the same PR.
See [`docs/stale-doc-policy.md`](docs/stale-doc-policy.md). Report leftover
drift with the
[Doc drift](https://github.com/boersmamarcel/chatty2/issues/new?template=doc-drift.yml)
issue template.

## Pull requests

1. Branch from `main`
2. Run `make ci` locally
3. Update docs when the [stale-doc policy](docs/stale-doc-policy.md) says to
4. For research crates, read [`RESERVED.md`](RESERVED.md) first

## Research work

Issues in **Self-improving chatty2** may have reserved symbols (`owner:human`,
`owner:pair`). Ordinary Chatty product work is unaffected.
