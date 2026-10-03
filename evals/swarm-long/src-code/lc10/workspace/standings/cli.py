"""Command line entry point.

    python3 -m standings.cli RESULTS [--deduct TEAM=POINTS ...] [--form N]
                                     [--title TEXT]

Prints the league table; with --form N a form column of the last N results
is appended to every line.
"""

import argparse
import sys

from .form import form
from .render import render
from .results import ResultError, parse_results
from .table import build_table
from .tiebreak import rank


def _deductions(items):
    deductions = {}
    for item in items or []:
        team, sep, points = item.rpartition("=")
        if not sep or not team.strip():
            raise ValueError("expected TEAM=POINTS, got %r" % (item,))
        deductions[" ".join(team.split())] = int(points)
    return deductions


def main(argv=None):
    parser = argparse.ArgumentParser(prog="standings")
    parser.add_argument("results")
    parser.add_argument("--deduct", action="append")
    parser.add_argument("--form", type=int, default=0)
    parser.add_argument("--title")
    args = parser.parse_args(argv)
    try:
        with open(args.results, encoding="utf-8") as handle:
            matches = parse_results(handle.read())
        rows = build_table(matches, deductions=_deductions(args.deduct))
    except (ResultError, ValueError) as exc:
        sys.stderr.write("standings: %s\n" % exc)
        return 2
    ranked = rank(rows, matches)
    lines = render(ranked, args.title).split("\n")
    if args.form > 0:
        offset = 2 if args.title else 0
        for index, row in enumerate(ranked):
            line_no = offset + 1 + index
            lines[line_no] = "%s  %s" % (lines[line_no], form(row.team, matches, args.form))
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    sys.exit(main())
