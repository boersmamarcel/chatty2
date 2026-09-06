# Make targets & CI workflows

**When to read this:** You want the exact command a `make` target runs, or which GitHub workflow does what and when it fires.

The Makefile mirrors `.github/workflows/ci.yml`; the workflow is the ground truth, and the two are kept in step by hand. Full context for the release pipeline is in the [Release process](./architecture/RELEASE_PROCESS.md).

## Make targets

| Target | Runs |
|--------|------|
| `make` / `make help` | Lists the targets below |
| `make setup` | Linux: `scripts/setup-linux.sh` (apt packages for GPUI + `rustup target add wasm32-wasip2`). Other OSes: prints instructions and adds the wasm target |
| `make build` | `cargo build` |
| `make build-release` | `cargo build --release` |
| `make test` | `cargo test --all-features -- --test-threads=1` (matches CI) |
| `make test-fast` | `cargo test -p chatty-core --lib` |
| `make test-tui` | `cargo test -p chatty-tui` |
| `make test-gpui` | `cargo test -p chatty-gpui` |
| `make test-gateway` | `cargo test -p chatty-protocol-gateway` |
| `make lint` | `cargo clippy --all-features -- -D warnings` |
| `make fmt` | `cargo fmt` |
| `make fmt-check` | `cargo fmt --check` |
| `make typecheck` | `cargo check --all-features` |
| `make wasm-modules` | Builds `modules/echo-agent` for `wasm32-wasip2` (release) and copies `echo_agent.wasm` next to its `module.toml` |
| `make run-gpui` | `cargo run -p chatty-gpui` |
| `make run-tui` | `cargo run -p chatty-tui` |
| `make ci` | `wasm-modules`, `test`, `fmt-check`, `lint`, `scripts/check-reserved.sh`, `scripts/check-rig-pins.sh` |
| `make clean` | `cargo clean` |
| `make docs-gen` | `scripts/gen-docs-reference.sh` → `docs/generated/*.md` |
| `make docs-sync` | `scripts/docs-sync.sh` — copies repo markdown (and docs-sized GIFs) into `docs-site/src` |
| `make docs` | `docs-gen` + `docs-sync` + `mdbook build docs-site`, then installs `llms.txt` / `llms-full.txt` into the book |
| `make docs-serve` | `docs-gen` + `docs-sync` + `mdbook serve docs-site --open` (port 3000) |
| `make docs-check-links` | `docs`, then `scripts/check-docs-links.sh` (lychee over the sources) and `scripts/check-docs-site-links.sh` (relative links on the built site) |
| `make docs-check-nav` | `scripts/check-docs-nav-drift.sh` — `docs/INDEX.md` and `SUMMARY.md` completeness |
| `make docs-check-frontmatter` | `scripts/check-docs-frontmatter.sh` — optional YAML frontmatter schema |
| `make docs-check-leakage` | `scripts/check-docs-user-leakage.sh` — user guides carry no contributor material |
| `make docs-check-reference` | `scripts/check-docs-reference-drift.sh` — reference tables match `tool_registry.rs`, `StreamManagerEvent`, event enums and settings structs |
| `make docs-check` | All five `docs-check-*` targets |
| `make animations` | `scripts/animations/record.sh --all` — re-records the README/docs GIFs |

> [!NOTE]
> Two things the CI workflow runs that `make ci` does not: clippy is invoked with `--all-targets` as well (`cargo clippy --all-features --all-targets -- -D warnings`, so lints in `tests/` are reported), and `scripts/check-no-core-reexports.sh` runs after the rig-pin check. Run both before pushing a change to test code or to the `chatty-gpui`/`chatty-tui` module trees.

## GitHub workflows

| Workflow | Trigger | Purpose |
|:---------|:--------|:--------|
| **CI** (`ci.yml`) | PR / push to `main` | Tests, formatting, clippy, reserved-symbol, rig-pin and re-export checks. Docs-only diffs skip compile (the required `test` job still passes). Stale PR runs are cancelled. `Swatinem/rust-cache` is warmed on `main` (never with `cache-on-failure`: a cancelled run would save a partial `target/` that is then never re-saved). On pushes to `main`, `warm-release-cache` also builds `--release` on Linux, macOS and Windows once per Cargo.lock + rustc and saves each under the key from `scripts/release-cache-key.sh`. |
| **Docs** (`docs.yml`) | Push to `main` / PR touching documentation paths | `gen-docs-reference.sh`, `docs-sync.sh`, then every check behind `make docs-check` (links on sources and built site, nav, frontmatter, user-guide leakage, reference drift); a `deploy` job publishes the site. |
| **Prepare Release** (`prepare-release.yml`) | PR merged with `release:patch`/`release:minor`/`release:major` label, or manual `workflow_dispatch` | Bumps version via a `cut-release` PR (main is protected), generates changelog, tags, creates the GitHub Release, then calls Release via `workflow_call`. |
| **Release** (`release.yml`) | Called by Prepare Release via `workflow_call`, or manual GitHub Release publish | Builds cross-platform artifacts (Linux AppImage, macOS DMG, Windows EXE), generates checksums, uploads to release. Restore-only cache: each platform restores the release cache that CI's `warm-release-cache` matrix built on `main`. |
| **Claude Code Review** (`claude-code-review.yml`) | PR opened/updated (Rust/CI paths only) | Automated AI code review via Claude. Skipped for docs-only PRs. |
| **Rig canary** (`rig-canary.yml`) | Weekly, or PR that touches Cargo manifests | Informational `cargo update` + `cargo check` against latest rig. |
| **Claude** (`claude.yml`) | `@claude` mention on issues/PRs | Interactive AI assistance. |
| **Update user docs** (`update-readme.yml`) | PR merged to `main` | Claude analyzes the diff; if user-facing features changed, opens a follow-up PR updating `docs-site/src/user/*.md` (README stays a short landing page, edited only for download/install/positioning/links). Add `skip-readme` label to opt out. |
| **Update Agent Docs** (`update-agent-docs.yml`) | PR merged to `main` | Claude analyzes the merged PR for guidance drift and opens a follow-up PR to sync `CLAUDE.md` / `AGENTS.md` when needed. Add `skip-agent-docs` label to opt out. |
| **UI Sync Check** (`ui-sync-check.yml`) | PR merged to `main` | Claude checks if one UI crate (chatty-gpui/chatty-tui) changed without the other; creates a `ui-sync` labeled issue if sync needed. Add `skip-sync-check` label to opt out. |
| **Dependency Check** (`dependency-check.yml`) | Weekly (Monday 9:00 UTC) or manual | Checks crates.io for dependency updates, files grouped work in Linear only (auto-ship, agent tech debt, or human tech debt track) — no GitHub issues; requires `LINEAR_API_KEY`. |

A merge only releases if the PR carried a release label *before* the merge; the label is read from the merged pull request. Prepare Release still shows a run for an unlabeled merge, with conclusion `skipped`. Details, including the docs-branch exclusion and the changelog rules, are in the [Release process](./architecture/RELEASE_PROCESS.md).
