#!/usr/bin/env bash
# Verifier for scripts/team-smoke.sh (AGE-408): judges the fixture repo as
# the leader left it. Runs inside the chatty-team-smoke container with the
# repo mounted at /work. Prints REWARD=1 and exits 0 only if every check
# holds on HEAD's working tree; otherwise REWARD=0 and exit 1.
set -u
cd "${1:-/work}" || exit 1
ok=1

echo "== git log:"
git --no-pager log --oneline --graph --all | head -8

unmerged=$(git branch --no-merged HEAD --list 'sub-agent/*' | tr -d ' *+' | tr '\n' ' ')
echo "== unmerged sub-agent/* branches: ${unmerged:-none}"
[ -z "$unmerged" ] || ok=0

echo "== tests on working tree:"
if ! python3 -m unittest discover -s tests -t . -v 2>&1 | tail -3; then ok=0; fi
python3 -m unittest discover -s tests -t . >/dev/null 2>&1 || ok=0

echo "== hidden checks:"
python3 - <<'PY' || ok=0
from bank import Account
a = Account(10)
assert a.deposit(5) == 15, "deposit must still return the balance"
b = Account(10)
for bad in (0, -1, 11):
    try:
        b.withdraw(bad)
        raise SystemExit("withdraw(%r) did not raise" % bad)
    except ValueError:
        pass
assert b.withdraw(4) == 6, "a successful withdrawal must return the new balance"
print("hidden checks passed")
PY

n=$(grep -c "def test_" tests/test_bank.py 2>/dev/null || echo 0)
echo "== tests defined: $n"
[ "$n" -ge 4 ] || ok=0

echo "REWARD=$ok"
[ "$ok" -eq 1 ]
