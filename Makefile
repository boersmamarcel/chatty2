# Convenience targets that mirror the commands run by .github/workflows/ci.yml.
# These exist for fast, single-command iteration; cargo remains the source of
# truth for build configuration.
#
# Usage:
#   make                # alias for `make help`
#   make ci             # everything CI runs, in order
#   make test           # full test suite (matches CI invocation)
#   make test-fast      # just chatty-core lib tests (inner loop)
#
# Behavior must not diverge from .github/workflows/ci.yml. When CI changes,
# update this file too.

.PHONY: help setup build build-release test test-fast test-tui test-gpui \
        test-gateway lint fmt fmt-check typecheck wasm-modules wasm-template \
        test-benford lint-module-sdk test-billing-sdk run-gpui \
        run-tui ci clean docs-gen docs-sync docs docs-serve docs-check-links \
        docs-check-nav docs-check-frontmatter docs-check-leakage \
        docs-check-reference docs-check animations

help:
	@echo "Common targets:"
	@echo "  make setup         Install Linux system deps + wasm32-wasip2 target"
	@echo "  make build         cargo build (debug)"
	@echo "  make build-release cargo build --release"
	@echo "  make test          Full test suite (matches CI: --all-features)"
	@echo "  make test-fast     cargo test -p chatty-core --lib (quick inner loop)"
	@echo "  make test-tui      cargo test -p chatty-tui (TUI changes only)"
	@echo "  make test-gpui     cargo test -p chatty-gpui (GPUI changes only)"
	@echo "  make test-gateway  cargo test -p chatty-protocol-gateway (gateway changes only)"
	@echo "  make lint          cargo clippy --all-features --all-targets -- -D warnings"
	@echo "  make fmt           cargo fmt"
	@echo "  make fmt-check     cargo fmt --check"
	@echo "  make typecheck     cargo check --all-features"
	@echo "  make wasm-modules  Build every WASM module and test fixture (needed by tests)"
	@echo "  make wasm-template cargo-generate a module from templates/module and build it (needs cargo-generate)"
	@echo "  make test-benford  benford-agent's own unit tests, on the host target"
	@echo "  make lint-module-sdk  clippy chatty-module-sdk for wasm32-wasip2"
	@echo "  make test-billing-sdk  hive-billing-sdk's tests, on the host target"
	@echo "  make run-gpui      cargo run -p chatty-gpui"
	@echo "  make run-tui       cargo run -p chatty-tui"
	@echo "  make docs-gen      Generate docs/generated reference pages"
	@echo "  make docs-sync     Copy repo markdown into docs-site/src"
	@echo "  make docs          docs-gen + docs-sync + mdbook build"
	@echo "  make docs-serve    Local mdBook preview (port 3000)"
	@echo "  make docs-check-links  Verify markdown links in sources (lychee) and on the built site"
	@echo "  make docs-check-nav    Verify INDEX.md + SUMMARY.md completeness (AGE-116)"
	@echo "  make docs-check-frontmatter  Validate optional doc YAML frontmatter (AGE-115)"
	@echo "  make docs-check-leakage  User guides must not carry contributor material"
	@echo "  make docs-check-reference  Reference tables match tool_registry.rs and friends"
	@echo "  make docs-check    All of the docs checks above"
	@echo "  make animations    Re-record the README/docs GIFs (scripts/animations/README.md)"
	@echo "  make ci            Everything CI runs, in order"

setup:
	@if [ "$$(uname -s)" = "Linux" ]; then \
		bash scripts/setup-linux.sh; \
	else \
		echo "Automated setup is only provided for Linux."; \
		echo "On macOS: install Xcode CLT, then 'rustup target add wasm32-wasip2'."; \
		echo "On Windows: see README.md and 'rustup target add wasm32-wasip2'."; \
		rustup target add wasm32-wasip2; \
	fi

build:
	cargo build

build-release:
	cargo build --release

# Matches the CI invocation exactly (AGE-600). PdfiumHandle
# (crates/chatty-core/src/services/pdfium_utils.rs, AGE-176) fixed the SIGTRAP
# that came from pdfium being used from several test threads at once, but CI
# still serializes (ci.yml's "Run tests" step): GitHub-hosted runners have
# shown other intermittent SIGTRAPs under parallel execution. `make test` was
# left parallel and had drifted from CI; match it here so a flake reproduces
# locally instead of only in CI.
test:
	cargo test --all-features -- --test-threads=1

# Fast inner loop: most logic lives in chatty-core. Use this while iterating
# on tools / services / settings models. Run `make test` before pushing.
test-fast:
	cargo test -p chatty-core --lib

# Per-crate test recipes — useful when you only touched one frontend.
# Run `make test` before pushing to verify the full suite still passes.
test-tui:
	cargo test -p chatty-tui

test-gpui:
	cargo test -p chatty-gpui

test-gateway:
	cargo test -p chatty-protocol-gateway

lint:
	cargo clippy --all-features --all-targets -- -D warnings

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

typecheck:
	cargo check --all-features

wasm-modules:
	scripts/build-wasm-fixtures.sh

# cargo-generate a module from templates/module and build it, the way the
# tutorials tell an author to (AGE-600). `modules/ci-generated` is
# git-ignored. Needs `cargo generate` (`cargo install cargo-generate`).
wasm-template:
	rm -rf modules/ci-generated
	cargo generate --path templates/module --name ci-generated \
		--define description=ci --destination modules --silent --no-workspace
	cargo build --manifest-path modules/ci-generated/Cargo.toml \
		--target wasm32-wasip2 --release

# benford-agent, chatty-module-sdk and hive-billing-sdk are standalone crates
# (their own `[workspace]`), so `make test`/`make lint` above never touch
# them. Both test crates default to `wasm32-wasip2` via their own
# `.cargo/config.toml`, which has no libtest runner, so the host target must
# be explicit (AGE-600).
test-benford:
	cargo test --manifest-path modules/benford-agent/Cargo.toml --target x86_64-unknown-linux-gnu

lint-module-sdk:
	cargo clippy --manifest-path crates/chatty-module-sdk/Cargo.toml --target wasm32-wasip2 -- -D warnings

test-billing-sdk:
	cargo test --manifest-path crates/hive-billing-sdk/Cargo.toml --target x86_64-unknown-linux-gnu

run-gpui:
	cargo run -p chatty-gpui

run-tui:
	cargo run -p chatty-tui

# Mirrors the Rust path in .github/workflows/ci.yml.
# GitHub skips this compile/test path when a PR only touches docs.
ci: wasm-modules wasm-template test test-benford lint-module-sdk test-billing-sdk
	$(MAKE) fmt-check
	$(MAKE) lint
	bash scripts/check-reserved.sh
	bash scripts/check-rig-pins.sh

clean:
	cargo clean

docs-gen:
	bash scripts/gen-docs-reference.sh

docs-sync:
	bash scripts/docs-sync.sh

docs: docs-gen docs-sync
	mdbook build docs-site
	install -m 644 docs/generated/llms.txt docs-site/book/llms.txt
	install -m 644 docs/generated/llms-full.txt docs-site/book/llms-full.txt

docs-serve: docs-gen docs-sync
	mdbook serve docs-site --open

docs-check-links: docs
	bash scripts/check-docs-links.sh
	bash scripts/check-docs-site-links.sh

docs-check-leakage:
	bash scripts/check-docs-user-leakage.sh

docs-check-reference:
	bash scripts/check-docs-reference-drift.sh

docs-check: docs-check-links docs-check-nav docs-check-frontmatter docs-check-leakage docs-check-reference

animations:
	bash scripts/animations/record.sh --all

docs-check-nav:
	bash scripts/check-docs-nav-drift.sh

docs-check-frontmatter:
	bash scripts/check-docs-frontmatter.sh
