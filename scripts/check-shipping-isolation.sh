#!/usr/bin/env bash
# AGE-26: shipping crates must not depend on chatty-optimize (or a resurrected chatty-eval).
# chatty-optimize is on-demand / CI-only research tooling — never a request-path dep.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
META=$(cargo metadata --format-version 1 --no-deps --locked)

check_no_dep() {
  local pkg=$1
  local forbidden=$2
  if printf '%s' "$META" | python3 -c "
import json, sys
meta = json.load(sys.stdin)
pkgs = {p['name']: p for p in meta['packages']}
p = pkgs.get('$pkg')
if p is None:
    # Package not in this metadata slice — ignore (workspace member list is source of truth).
    sys.exit(0)
deps = {d['name'] for d in p['dependencies']}
sys.exit(0 if '$forbidden' not in deps else 1)
"; then
    :
  else
    echo "ISOLATION VIOLATION: $pkg must not depend on $forbidden"
    fail=1
  fi
}

# Direct Cargo.toml deps (no-deps metadata still lists declared dependencies).
for pkg in chatty-trace chatty-playbook chatty-flow chatty-core chatty-gpui chatty-tui; do
  check_no_dep "$pkg" chatty-optimize
  check_no_dep "$pkg" chatty-eval
done

# Full graph: shipping app binaries must not pull chatty-optimize transitively either.
FULL=$(cargo metadata --format-version 1 --locked)
printf '%s' "$FULL" | python3 -c "
import json, sys
meta = json.load(sys.stdin)
resolve = meta.get('resolve') or {}
nodes = {n['id']: n for n in resolve.get('nodes', [])}
pkgs = {p['id']: p for p in meta['packages']}
name_of = {pid: p['name'] for pid, p in pkgs.items()}

def reachable(root_name: str) -> set[str]:
    roots = [pid for pid, name in name_of.items() if name == root_name]
    if not roots:
        return set()
    seen = set()
    stack = list(roots)
    while stack:
        cur = stack.pop()
        if cur in seen:
            continue
        seen.add(cur)
        node = nodes.get(cur)
        if not node:
            continue
        for dep in node.get('dependencies', []):
            stack.append(dep)
    return {name_of[pid] for pid in seen if pid in name_of}

fail = 0
for root in ('chatty-gpui', 'chatty-tui', 'chatty-core', 'chatty-trace', 'chatty-playbook', 'chatty-flow'):
    names = reachable(root)
    for bad in ('chatty-optimize', 'chatty-eval'):
        if bad in names and bad != root:
            print(f'ISOLATION VIOLATION: {root} resolve graph includes {bad}')
            fail = 1
sys.exit(fail)
" || fail=1

if [ "$fail" -eq 0 ]; then
  echo "shipping-isolation check: OK"
fi
exit "$fail"
