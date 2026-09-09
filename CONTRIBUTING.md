# Contributing to Chatty

Thank you for contributing to [chatty2](https://github.com/boersmamarcel/chatty2).

Start with the developer guide on the docs site:
[Build and run in 10 minutes](https://boersmamarcel.github.io/chatty2/dev/start/build-and-run.html),
then [Contributing patterns](https://boersmamarcel.github.io/chatty2/dev/contributing-patterns.html).
In the repo, [`AGENTS.md`](AGENTS.md) is the workspace map, and
[`docs/INDEX.md`](docs/INDEX.md) lists every architecture page.

## Development workflow

```bash
make setup        # once on Linux
make wasm-modules # before the full test suite
make ci           # same compile/test/lint path GitHub runs for Rust PRs
```

A full `cargo test --all-features` needs about 16 GiB of `target/` even
with the workspace's trimmed debuginfo profile; see
[`docs/build-disk-usage.md`](docs/build-disk-usage.md) before building on a
small disk.

## Pull requests

1. Branch from `main`.
2. Run `make ci` locally.
3. Update the docs in the same PR when your change alters a fact a page
   claims (see below).
4. For research crates, read [`RESERVED.md`](RESERVED.md) first.

## Documentation

The site is built from the repo: `docs/*.md` (architecture), `docs/research/`
(ADRs), `AGENTS.md`, `crates/*/README.md`, plus hand-written pages under
`docs-site/src/`. Edit the source, never the copy under `docs-site/src/dev/`.
`make docs-serve` previews the site; `make docs-check` runs the same checks
as CI (links on the built site, nav completeness, frontmatter, user-guide
leakage, reference drift). Docs-only pull requests do not compile the workspace.

**Same-PR rule.** If a change alters a fact a page claims, update that page in
the same PR. A page is stale when someone following it would do the wrong
thing, look in the wrong place, or miss a step. Internal refactors that keep
documented facts true, formatting, comments and dependency bumps need no doc
change.

| If you change… | Update… |
|---|---|
| Architecture, crate boundaries, or a documented pattern | The matching `docs/*.md` page |
| A how-to's steps or file paths | `docs-site/src/dev/guides/` |
| User-visible behaviour a user guide describes | `docs-site/src/user/` (and `README.md` if the landing page claims it) |
| CI commands, make targets, or contributor conventions | `AGENTS.md` and the CI reference page |
| A crate's purpose or public surface | That crate's `README.md` |
| A tool, event, setting or env var | The tables in `scripts/gen-docs-reference.sh` (CI diffs them against source) |
| A new file under `docs/` | A row in `docs/INDEX.md` and an entry in `docs-site/src/SUMMARY.md` |

Point-in-time plans, audits and bug lists go to `docs/archive/` (not synced
to the site) or to Linear, not under `docs/`.

Workflows (`update-agent-docs.yml`, `update-readme.yml`) open follow-up PRs
after a merge to `main` when guidance drifted; they are a safety net, not a
reason to skip the same-PR rule. Found a stale page you are not fixing? File
a [Doc drift](https://github.com/boersmamarcel/chatty2/issues/new?template=doc-drift.yml)
issue.

## Research work

Issues in **Self-improving chatty2** may have reserved symbols (`owner:human`,
`owner:pair`). Ordinary Chatty product work is unaffected.
