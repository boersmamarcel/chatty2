# verspec

Semantic versions for release tooling (Python 3, standard library only):
parsing and ordering, constraints (`>=`, `^`, `~`, wildcards), version
bumps, picking the best published version, and lock files.

    python3 -m verspec.cli max '^1.2.0' 1.2.0 1.2.5 1.3.0
    python3 -m verspec.cli bump 1.2.3 minor
    python3 -m verspec.cli check verspec.lock constraints.txt

See the module docstrings in `verspec/` for the details, and `ISSUES.md`
for the open issues.

## Tests

    python3 -m unittest discover -s tests -t .
