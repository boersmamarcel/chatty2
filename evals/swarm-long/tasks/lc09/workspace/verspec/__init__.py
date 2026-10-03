"""verspec: semantic versions, constraints and lock files.

Modules:

    version      -- parsing, printing and ordering of versions
    constraints  -- version constraints (>=, <, ^, ~, wildcards)
    bump         -- computing the next version
    resolve      -- picking versions from a list of published versions
    lockfile     -- reading, writing and comparing lock files
    cli          -- command line entry point (python3 -m verspec.cli)
"""

__version__ = "0.9.2"
