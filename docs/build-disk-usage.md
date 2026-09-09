# Build disk usage

Chatty is a large workspace: `Cargo.lock` pins ~1,400 crates, several of
them enormous (wasmtime, typst, gpui + tree-sitter grammars, chromiumoxide's
generated CDP types, and the bundled DuckDB C++ build). A full
`cargo test --all-features` at Cargo's defaults needs **more than 28 GiB**
of `target/` and will not finish on a machine with 30 GB free.

This page explains where the space goes, what the workspace does about it,
and how to keep a local checkout small.

## Where the space goes

Measured on Linux x86_64 with `cargo test --all-features --no-run` (the same
build CI runs), before and after the profile settings in the root
`Cargo.toml`. The baseline run died with `No space left on device` while
linking the desktop binary and its tests, so its real total is higher than
shown; the five binaries it never linked (the desktop binary, its unit
tests, and three integration-test binaries) are the largest ones, so a
complete baseline would land well past 30 GiB. The middle column is the
profile without `split-debuginfo`, measured separately to show what each
setting buys.

| `target/debug/` component | Cargo defaults | Dep debuginfo trimmed | + split debuginfo (current) |
|---|---|---|---|
| Total | > 27.5 GiB (died while linking; 12 of 17 binaries) | 18.5 GiB, complete | 15.8 GiB, complete (17.1 GiB after `cargo clippy --all-features`) |
| `deps/*.rlib` (compiled dependencies) | 9.3 GiB | 5.2 GiB | 5.0 GiB (+ 0.9 GiB of `.dwo`) |
| `deps/` test + bin executables | 6.8 GiB for 12 of 17 | 5.4 GiB for all 17 | 3.3 GiB for all 17 |
| `build/libduckdb-sys-*` (DuckDB C++ objects) | 4.6 GiB | 1.0 GiB | 1.0 GiB |
| `incremental/` (workspace crates only) | 2.8 GiB (gpui crate unfinished) | 3.1 GiB | 2.7 GiB |
| Largest single file | `liblibduckdb_sys.rlib`, 2.3 GiB | `chatty` desktop binary, 1.3 GiB | `chatty` desktop binary, 0.9 GiB |
| Wall clock | 28 min to the failure | 17 min | 15.5 min |

Two things dominate at the defaults:

1. **Full DWARF for every dependency.** The dev profile defaults to
   `debug = 2`, so each of the 1,400 rlibs carries complete debuginfo, and
   every executable that links them gets its own copy. The `chatty-core`
   unit-test binary alone was 2.2 GiB.
2. **DuckDB compiled with `-g`.** `libduckdb-sys` builds DuckDB from source
   through the `cc` crate, which passes `-g` whenever the package profile has
   any debuginfo. Objects plus rlib came to ~7 GiB.

## What the workspace does

The root `Cargo.toml` sets, for the `dev` profile (and therefore `test`,
which inherits it):

```toml
[profile.dev]
split-debuginfo = "unpacked"

[profile.dev.package."*"]
debug = "line-tables-only"

[profile.dev.package.libduckdb-sys]
debug = false
```

Workspace crates keep full debuginfo, so stepping through Chatty code in a
debugger is unchanged. Dependencies keep line tables, so panics and
`RUST_BACKTRACE=1` still print `file:line` inside them; what you lose is
variable inspection *inside* third-party code. DuckDB gets no debuginfo at
all.

`split-debuginfo = "unpacked"` keeps whatever debuginfo remains in per-crate
`.dwo` files under `target/debug/deps/` instead of copying it into every
linked executable; the 17 test and bin binaries then share one set. This is
already Cargo's default on macOS. Backtraces (`RUST_BACKTRACE=1`), gdb and
lldb all read the `.dwo` files in place; only the files under `target/` are
needed, nothing else to install. If you copy a test binary elsewhere to
debug it, copy `target/debug/deps/*.dwo` along with it.

Release builds are untouched.

## Keeping a local checkout small

- **Share one `target/` across worktrees and clones.** Every checkout gets
  its own `target/` by default, so three worktrees cost three full builds.
  Point them at one directory with `CARGO_TARGET_DIR` (for example in
  `~/.cargo/config.toml` under `[build] target-dir = "/path/to/shared-target"`).
- **Don't mix profiles you don't need.** `cargo build --release` writes a
  second, separate tree under `target/release/`. Only build release locally
  when packaging.
- **Prune stale artifacts.** Every dependency bump leaves the old version's
  artifacts behind until you clean. `cargo clean` drops everything;
  [`cargo-sweep`](https://crates.io/crates/cargo-sweep) (`cargo sweep --time 14`)
  removes only artifacts unused for N days and keeps the warm cache.
- **Keep `CARGO_INCREMENTAL` on locally, off in CI.** Incremental state is
  only kept for workspace crates and is what makes edit-compile loops fast.
  CI already sets `CARGO_INCREMENTAL=0` because a fresh runner never reuses
  it.
- **The registry cache lives outside `target/`.** `~/.cargo/registry` holds
  ~2 GiB of crate sources for this lockfile. `cargo cache --autoclean`
  (from [`cargo-cache`](https://crates.io/crates/cargo-cache)) trims old
  versions.

## If you need full dependency debuginfo for one session

Override on the command line without editing `Cargo.toml`:

```bash
# one dependency
cargo build --config 'profile.dev.package.wasmtime.debug=2' -p chatty-wasm-runtime
# everything (expect the "Cargo defaults" column above)
cargo build --config 'profile.dev.package."*".debug=2'
```
