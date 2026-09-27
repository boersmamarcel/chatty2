#!/usr/bin/env bash
# Build every WASM test fixture plus the two real modules, and stage them for
# the tests (AGE-596).
#
#   target/wasm-fixtures/<name>/<name>.wasm
#   target/wasm-fixtures/<name>/module.toml
#
# One directory per module, since a registry-loadable module is a directory
# holding its own module.toml. `chatty_wasm_runtime::test_support::fixture_path`
# reads from here. Every crate builds into one shared target directory
# (target/wasm-fixtures/.build) so the SDK and wit-bindgen compile once.
#
# Idempotent; exits non-zero on the first failure. Needs the wasm32-wasip2
# target (`rustup target add wasm32-wasip2`).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/target/wasm-fixtures"
build="$out/.build"
mkdir -p "$out"

# name → crate directory. The .wasm cargo writes is the package name with
# `-`/`.` as `_`.
crates=(
  "echo-agent:modules/echo-agent"
  "benford-agent:modules/benford-agent"
  "wit-0.1:modules/fixtures/wit-0.1"
)
for dir in "$root"/modules/fixtures/*/; do
  name="$(basename "$dir")"
  [[ -f "$dir/Cargo.toml" && "$name" != "wit-0.1" ]] && crates+=("$name:modules/fixtures/$name")
done

stage() {
  local name="$1" wasm="$2" manifest_dir="$3"
  local dest="$out/$name"
  mkdir -p "$dest"
  cp "$wasm" "$dest/$name.wasm"
  # A module.toml names its own wasm file; point it at the staged name.
  sed -E "s|^wasm = .*|wasm = \"$name.wasm\"|" "$manifest_dir/module.toml" > "$dest/module.toml"
}

for entry in "${crates[@]}"; do
  name="${entry%%:*}"
  dir="$root/${entry#*:}"
  package="$(sed -nE 's/^name = "(.*)"/\1/p' "$dir/Cargo.toml" | head -1)"
  echo "==> $name ($package)"
  cargo build --manifest-path "$dir/Cargo.toml" --target wasm32-wasip2 --release \
    --target-dir "$build"
  stage "$name" "$build/wasm32-wasip2/release/${package//[-.]/_}.wasm" "$dir"
done

# Not buildable with cargo: a plain core module, committed beside its source.
echo "==> core-module (prebuilt from core_module.wat)"
stage core-module "$root/modules/fixtures/core-module/core_module.wasm" \
  "$root/modules/fixtures/core-module"

echo "Staged $(find "$out" -mindepth 2 -maxdepth 2 -name '*.wasm' | wc -l) modules in $out"
