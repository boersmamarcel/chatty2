#!/usr/bin/env bash
# AGE-26: verify exact-minor pins for the rig 0.42 family in the workspace Cargo.toml.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
check_pin() {
  local name=$1
  if ! grep -E "^${name} = \"=" Cargo.toml >/dev/null; then
    # Also allow table form: name = { version = "=x.y.z", ... }
    if ! grep -E "^${name} = \{ version = \"=" Cargo.toml >/dev/null; then
      echo "RIG PIN VIOLATION: $name must be exact-minor (=x.y.z), no caret ranges"
      fail=1
    fi
  fi
}

check_pin rig-core
check_pin rig-agent
check_pin rig-mcp
check_pin rig-tap

if [ "$fail" -eq 0 ]; then
  echo "rig-pin check: OK"
fi
exit "$fail"
