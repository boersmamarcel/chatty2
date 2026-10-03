"""standings: league tables from match results.

Modules:

    results   -- the Match record and the results file parser
    table     -- per-team rows (played, won, ..., points), point rules
    tiebreak  -- ordering the table, including head-to-head
    form      -- recent form strings such as 'WWDLW'
    render    -- the fixed-width text table
    cli       -- command line entry point (python3 -m standings.cli)
"""

__version__ = "2.1.0"
