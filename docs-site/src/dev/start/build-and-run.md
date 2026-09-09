# Build and run in 10 minutes

**When to read this:** You have just cloned the repository and want the desktop app, the terminal app and the test suite working before you touch any code.

## Goal

A compiled workspace, the terminal app answering a prompt from a local Ollama model with no API key, the desktop app open, and the same checks CI runs passing on your machine.

## Prerequisites

- **Rust** via [rustup](https://rustup.rs). The workspace is edition 2024 and declares `rust-version = "1.94"` in the root `Cargo.toml`; `rustup default stable` satisfies it.
- **Git** and a checkout of <https://github.com/boersmamarcel/chatty2>.
- **Linux**: Debian/Ubuntu packages for GPUI (`make setup` installs them). **macOS**: Xcode Command Line Tools. **Windows**: see the repo `README.md`.
- Optional: [Ollama](https://ollama.com) for a zero-configuration local model.

> [!WARNING]
> A full `cargo test --all-features` needs about **16 GiB of `target/`** even with the workspace's trimmed debuginfo profile (at Cargo's defaults it is more than 28 GiB and does not finish on a 30 GB disk). Read [Build disk usage](../architecture/build-disk-usage.md) before building on a small disk; sharing one `CARGO_TARGET_DIR` across worktrees is the biggest single saving.

## Steps

### 1. Clone and install system dependencies

```bash
git clone https://github.com/boersmamarcel/chatty2.git
cd chatty2
make setup
```

On Linux, `make setup` runs [`scripts/setup-linux.sh`](https://github.com/boersmamarcel/chatty2/blob/main/scripts/setup-linux.sh): the `lib*-dev` package list from CI plus `rustup target add wasm32-wasip2`. On macOS and Windows it prints what to install by hand and adds the `wasm32-wasip2` target.

### 2. Build the echo-agent WASM module

```bash
make wasm-modules
```

Some integration tests load `modules/echo-agent/echo_agent.wasm`. The file is git-ignored, so it has to be built once after every fresh clone (and again if you `cargo clean` inside `modules/echo-agent`).

### 3. Run the terminal app headless against Ollama

No provider configuration is needed: `--ollama` queries a running Ollama for its models and injects a provider and model list for the session only. Nothing is written to disk.

```bash
ollama serve                      # if it is not already running
ollama pull qwen2.5:0.5b          # any small model works
cargo run -p chatty-tui -- --ollama --model qwen2.5:0.5b --headless -m "Say hello in five words"
```

`--ollama` with no value means `http://localhost:11434`; pass a URL for a remote instance (`--ollama http://remote:11434`). `--headless` prints the full response to stdout and exits; logging is suppressed so stdout stays clean. Two related modes:

```bash
# Pipe mode: stdin is the prompt
cat src/main.rs | cargo run -p chatty-tui -- --ollama --model qwen2.5:0.5b --pipe

# Interactive TUI
cargo run -p chatty-tui -- --ollama
```

Without `--ollama` (or `--openai-compat-url`), `chatty-tui` reads the providers and models the desktop app saved, so either run the desktop app once and add a provider, or keep using the direct-connect flags. `cargo run -p chatty-tui -- --help` lists every flag; the [CLI reference](../reference/cli-flags.md) summarises them.

### 4. Run the desktop app

```bash
cargo run -p chatty-gpui          # or: make run-gpui
```

The desktop app detects a running local Ollama and lists its models, so the same zero-configuration setup works here. For hosted models, open **Settings → Models** and add an OpenRouter API key.

On a Linux machine with no session bus (a headless VM, a container), export `XDG_RUNTIME_DIR` first or X11 window creation fails with `BadMatch`:

```bash
export XDG_RUNTIME_DIR=/tmp/xdg-runtime-$(id -u) && mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
```

### 5. Run the fast tests

```bash
make test-fast                    # cargo test -p chatty-core --lib
```

Most logic lives in `chatty-core`, so this is the inner loop while you iterate on tools, services and settings models. It needs no display server.

### 6. Run what CI runs

```bash
make ci
```

`make ci` is `make wasm-modules`, then `cargo test --all-features -- --test-threads=1`, `cargo fmt --check`, `cargo clippy --all-features -- -D warnings`, and the reserved-symbol and rig-pin checks. The full suite is the one that needs the disk space above. [Make targets & CI workflows](../ci-reference.md) lists every target.

## Verify

- Step 3 printed a reply and exited with status 0.
- Step 4 opened a window with your Ollama models in the model picker.
- Step 5 and step 6 finished green.

## Checklist

- [ ] `make setup` ran (or the equivalent packages and the `wasm32-wasip2` target are installed)
- [ ] `make wasm-modules` produced `modules/echo-agent/echo_agent.wasm`
- [ ] `chatty-tui --headless` answered a prompt
- [ ] `cargo run -p chatty-gpui` opened the desktop app
- [ ] `make test-fast` and `make ci` pass

## Common mistakes

| Symptom | Fix |
|---------|-----|
| A test fails with a missing `echo_agent.wasm` | `make wasm-modules` |
| `feature edition2024 is required` | Toolchain too old; `rustup default stable` |
| `No models configured` from `chatty-tui` | Add `--ollama` (or `--openai-compat-url`), or configure a provider in the desktop app first |
| `Could not connect to Ollama` | `ollama serve` is not running, or the URL is wrong |
| `BadMatch` when the desktop window opens on Linux | Export `XDG_RUNTIME_DIR` as in step 4 |
| `No space left on device` during `make ci` | [Build disk usage](../architecture/build-disk-usage.md); share a `CARGO_TARGET_DIR` |
| `fatal error: 'memory' file not found` or `unable to find library -lstdc++` | `cc`/`c++` resolve to clang; point them at `gcc`/`g++` (`update-alternatives` on Debian/Ubuntu) |
| Tests pass locally but SIGTRAP in CI | Re-run with `--test-threads=1`; see [Test](../guides/test.md) |

## Next

- [Your first change: add a tool](./first-change.md)
- [Where do I…?](../where-to-look.md) — the how-to index
- [Contributing patterns](../contributing-patterns.md) — the conventions the code follows
