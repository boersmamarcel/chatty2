#!/usr/bin/env bash
# The chatty × Hive nightly (PL-E6, AGE-601), runnable locally the same way
# `.github/workflows/plugin-e2e.yml` runs it.
#
#   scripts/hive-e2e.sh build    build hive-registry + hive-runner from $HIVE_DIR,
#                                chatty-wasm-runtime patched to this checkout
#   scripts/hive-e2e.sh up       bring the compose stack up (e2e profile) and wait
#   scripts/hive-e2e.sh seed     publish target/wasm-fixtures with hive's seed-e2e.sh
#   scripts/hive-e2e.sh test     cargo test -p hive-e2e -- --ignored; writes the
#                                report to $HIVE_E2E_REPORT
#   scripts/hive-e2e.sh down     docker compose down -v (this project only)
#   scripts/hive-e2e.sh all      build, up, seed, test, down
#
# Env:
#   HIVE_DIR              a hive checkout (required for build/up/seed/down)
#   HIVE_TARGET_DIR       cargo target dir for the hive build (default $HIVE_DIR/target)
#   HIVE_E2E_PROJECT      compose project name (default hive-e2e); never reuse
#                         another stack's name: `down` removes its volumes
#   HIVE_REGISTRY_PORT    host port of the registry (default 8080)
#   HIVE_RUNNER_PORT      host port of the runner (default 8081)
#   HIVE_E2E_REPORT       markdown report path (default target/hive-e2e-report.md)
#
# Needs: cargo, docker compose v2.24+ (`!reset`), curl, jq, and the fixtures
# (scripts/build-wasm-fixtures.sh).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
project="${HIVE_E2E_PROJECT:-hive-e2e}"
registry_port="${HIVE_REGISTRY_PORT:-8080}"
runner_port="${HIVE_RUNNER_PORT:-8081}"
report="${HIVE_E2E_REPORT:-$root/target/hive-e2e-report.md}"

need_hive() {
  : "${HIVE_DIR:?set HIVE_DIR to a hive checkout}"
  hive_target="${HIVE_TARGET_DIR:-$HIVE_DIR/target}"
  bin_dir="$hive_target/debug"
}

compose() {
  need_hive
  HIVE_E2E_BIN_DIR="$bin_dir" \
    HIVE_REGISTRY_PORT="$registry_port" HIVE_RUNNER_PORT="$runner_port" \
    LLM_UPSTREAM_URL=http://llm-upstream:8080/v1 \
    docker compose \
    -f "$HIVE_DIR/docker/docker-compose.yml" \
    -f "$root/crates/hive-e2e/compose.prebuilt.yml" \
    -p "$project" --profile e2e "$@"
}

build() {
  need_hive
  # hive-runner pins chatty-wasm-runtime by rev from the `.git` URL; patch
  # that source to this checkout so ABI drift between chatty2 main and hive
  # fails here. `cargo update` moves the lock entry onto the patch (a patch
  # alone is ignored while the lock still names the pinned version).
  local patch="patch.\"https://github.com/boersmamarcel/chatty2.git\".chatty-wasm-runtime.path=\"$root/crates/chatty-wasm-runtime\""
  (
    cd "$HIVE_DIR"
    locked="$(grep -A2 '^name = "chatty-wasm-runtime"' Cargo.lock |
      grep -B1 'chatty2.git?rev=' | sed -n 's/^version = "\(.*\)"/\1/p' | head -1 || true)"
    if [[ -n "$locked" ]]; then
      cargo update --config "$patch" -p "chatty-wasm-runtime@$locked"
    fi
    CARGO_TARGET_DIR="$hive_target" cargo build --config "$patch" -p hive-registry -p hive-runner
    cargo tree --config "$patch" -p hive-runner --depth 1 |
      grep -F "chatty-wasm-runtime" | grep -qF "($root/crates/chatty-wasm-runtime)" ||
      { echo "hive-e2e.sh: hive-runner is not built against $root/crates/chatty-wasm-runtime" >&2; exit 1; }
  )
  echo "[hive-e2e] built $bin_dir/hive-registry and $bin_dir/hive-runner against this checkout" >&2
}

up() {
  compose up -d --no-build
  curl -sf --retry 30 --retry-all-errors --retry-delay 2 "http://localhost:$registry_port/health" >/dev/null
  curl -sf --retry 30 --retry-all-errors --retry-delay 2 "http://localhost:$runner_port/health" >/dev/null
  echo "[hive-e2e] stack $project up: registry :$registry_port, runner :$runner_port" >&2
}

seed() {
  need_hive
  HIVE_E2E_BASE_URL="http://localhost:$registry_port" \
    "$HIVE_DIR/scripts/seed-e2e.sh" "$root/target/wasm-fixtures"
}

run_tests() {
  local data log status
  data="$(mktemp -d)"
  log="$(mktemp)"
  mkdir -p "$(dirname "$report")"
  set +e
  XDG_DATA_HOME="$data" \
    HIVE_E2E_BASE_URL="http://localhost:$registry_port" \
    HIVE_E2E_RUNNER_URL="http://localhost:$runner_port" \
    cargo test -p hive-e2e --tests --no-fail-fast -- --ignored --test-threads=1 2>&1 | tee "$log"
  status="${PIPESTATUS[0]}"
  set -e
  write_report "$log" >"$report"
  echo "[hive-e2e] report: $report" >&2
  rm -rf "$data"
  return "$status"
}

# One row per S5/S6 test (`s5_07_…` is row 5.7), then each failure's own
# message, which names the finding and the PL-H issue that fixes it.
write_report() {
  local log="$1"
  echo "## Hive contract and trust suite (S5, S6)"
  echo
  echo "chatty2 \`$(git -C "$root" rev-parse --short HEAD)\` × hive \`$(git -C "${HIVE_DIR:-$root}" rev-parse --short HEAD 2>/dev/null || echo '?')\`"
  echo
  echo "| Row | Test | Result |"
  echo "| -- | -- | -- |"
  sed -nE 's/^test (s([56])_([0-9]+)_[a-z0-9_]+) \.\.\. (ok|FAILED)$/\2.\3 \1 \4/p' "$log" |
    sort -t. -k1,1n -k2,2n |
    while read -r row name result; do
      row="${row%%.*}.$((10#${row#*.}))"
      [[ "$result" == ok ]] && result="pass" || result="**red**"
      echo "| $row | \`$name\` | $result |"
    done
  if grep -q '^---- s[56]_' "$log"; then
    echo
    echo "### Failures"
    echo
    echo '```'
    awk '/^---- s[56]_/{p=1} /^failures:$/{p=0} p' "$log" | grep -v '^note: run with'
    echo '```'
  fi
}

down() {
  compose down -v
}

case "${1:-}" in
build) build ;;
up) up ;;
seed) seed ;;
test) run_tests ;;
down) down ;;
all)
  build
  trap down EXIT
  up
  seed
  run_tests
  ;;
*)
  sed -n '2,24p' "$0"
  exit 2
  ;;
esac
