#!/usr/bin/env bash
# Fail if the repository root holds a tracked entry that is neither on the
# allowlist below nor a dotfile/dot-directory. Usage:
#   scripts/check-repo-root.sh              check ${REPO_ROOT:-.}
#   scripts/check-repo-root.sh --self-test  prove the check passes and fails correctly
set -euo pipefail

# The one allowlist. Add an entry here on purpose; names starting with "." are always allowed.
ALLOWED_ROOT=(
  AGENTS.md CLAUDE.md CONTRIBUTING.md Cargo.lock Cargo.toml LICENSE Makefile
  README.md RESERVED.md assets crates deny.toml docs docs-site modules scripts
  skills-lock.json templates themes wit
)

check() {
  local root="${REPO_ROOT:-.}" name allowed a listing strays=()
  listing="$(git -C "$root" ls-tree --name-only HEAD)"
  while IFS= read -r name; do
    [[ -z "$name" || "$name" == .* ]] && continue
    allowed=0
    for a in "${ALLOWED_ROOT[@]}"; do
      [[ "$name" == "$a" ]] && { allowed=1; break; }
    done
    [[ $allowed -eq 1 ]] || strays+=("$name")
  done <<< "$listing"
  if [[ ${#strays[@]} -gt 0 ]]; then
    for name in "${strays[@]}"; do
      echo "repo root: '$name' is not on the allowlist in scripts/check-repo-root.sh; move it, or add it there on purpose"
    done
    exit 1
  fi
}

self_test() {
  local self out
  self="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
  tmp="$(mktemp -d)" # global: the EXIT trap runs after this function returns
  trap 'rm -rf "$tmp"' EXIT
  git -C "$tmp" init -q
  git -C "$tmp" config user.name selftest
  git -C "$tmp" config user.email selftest@example.invalid
  echo x > "$tmp/README.md"
  git -C "$tmp" add -A && git -C "$tmp" commit -q -m base

  REPO_ROOT="$tmp" bash "$self" >/dev/null 2>&1 \
    || { echo "self-test: clean repo should pass" >&2; exit 1; }

  echo x > "$tmp/foo.py"
  git -C "$tmp" add -A && git -C "$tmp" commit -q -m stray
  if out="$(REPO_ROOT="$tmp" bash "$self" 2>&1)"; then
    echo "self-test: stray foo.py should fail" >&2; exit 1
  fi
  [[ "$out" == *foo.py* ]] || { echo "self-test: failure output must name foo.py" >&2; exit 1; }

  echo x > "$tmp/.hidden"
  git -C "$tmp" rm -q foo.py
  git -C "$tmp" add -A && git -C "$tmp" commit -q -m dot
  REPO_ROOT="$tmp" bash "$self" >/dev/null 2>&1 \
    || { echo "self-test: dotfile should pass" >&2; exit 1; }
  echo "self-test: ok"
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
else
  check
fi
