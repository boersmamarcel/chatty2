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
        test-gateway lint fmt fmt-check typecheck wasm-modules run-gpui \
        run-tui ci clean

help:
	@echo "Common targets:"
	@echo "  make setup         Install Linux system deps + wasm32-wasip2 target"
	@echo "  make build         cargo build (debug)"
	@echo "  make build-release cargo build --release"
	@echo "  make test          Full test suite (matches CI: --all-features --test-threads=1)"
	@echo "  make test-fast     cargo test -p chatty-core --lib (quick inner loop)"
	@echo "  make test-tui      cargo test -p chatty-tui (TUI changes only)"
	@echo "  make test-gpui     cargo test -p chatty-gpui (GPUI changes only)"
	@echo "  make test-gateway  cargo test -p chatty-protocol-gateway (gateway changes only)"
	@echo "  make lint          cargo clippy -- -D warnings"
	@echo "  make fmt           cargo fmt"
	@echo "  make fmt-check     cargo fmt --check"
	@echo "  make typecheck     cargo check --all-features"
	@echo "  make wasm-modules  Build modules/echo-agent for wasm32-wasip2 (needed by tests)"
	@echo "  make run-gpui      cargo run -p chatty-gpui"
	@echo "  make run-tui       cargo run -p chatty-tui"
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

# Matches the CI invocation exactly. --test-threads=1 is a workaround for
# intermittent SIGTRAPs in chatty-core under parallel execution on
# GitHub-hosted runners; see .github/workflows/ci.yml.
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
	cargo clippy -- -D warnings

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

typecheck:
	cargo check --all-features

wasm-modules:
	cd modules/echo-agent && cargo build --target wasm32-wasip2 --release \
		&& cp target/wasm32-wasip2/release/echo_agent.wasm .

run-gpui:
	cargo run -p chatty-gpui

run-tui:
	cargo run -p chatty-tui

# Mirrors the order in .github/workflows/ci.yml.
ci: wasm-modules test
	cargo build -p chatty-tui
	./target/debug/chatty-tui --help
	$(MAKE) fmt-check
	$(MAKE) lint

clean:
	cargo clean
