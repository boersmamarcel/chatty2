"""Command line entry point.

    python3 -m verspec.cli sort VERSION...
    python3 -m verspec.cli max CONSTRAINT VERSION... [--pre]
    python3 -m verspec.cli bump VERSION PART [--preid ID]
    python3 -m verspec.cli check LOCKFILE CONSTRAINTS

CONSTRAINTS is a file with one `name constraint` per line, e.g.
`requests >=2.28, <3`. `check` prints one line per problem and exits 1 when
there is any.
"""

import argparse
import sys

from .bump import PARTS, bump
from .constraints import ConstraintError, parse_constraint
from .lockfile import LockError, check_lock, parse_lock
from .resolve import max_satisfying, sort_versions
from .version import VersionError


def _read(path):
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def parse_constraints_file(text):
    """Dict name -> Constraint from 'name constraint' lines."""
    constraints = {}
    for line_no, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        name, _, spec = line.partition(" ")
        if not spec.strip():
            raise ConstraintError("line %d: expected '<name> <constraint>'" % line_no)
        constraints[name.lower()] = parse_constraint(spec)
    return constraints


def build_parser():
    parser = argparse.ArgumentParser(prog="verspec")
    sub = parser.add_subparsers(dest="command")
    p = sub.add_parser("sort")
    p.add_argument("versions", nargs="+")
    p = sub.add_parser("max")
    p.add_argument("constraint")
    p.add_argument("versions", nargs="+")
    p.add_argument("--pre", action="store_true")
    p = sub.add_parser("bump")
    p.add_argument("version")
    p.add_argument("part", choices=PARTS)
    p.add_argument("--preid", default="rc")
    p = sub.add_parser("check")
    p.add_argument("lockfile")
    p.add_argument("constraints")
    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    try:
        if args.command == "sort":
            print("\n".join(str(v) for v in sort_versions(args.versions)))
        elif args.command == "max":
            best = max_satisfying(args.versions, parse_constraint(args.constraint), args.pre)
            if best is None:
                sys.stderr.write("verspec: no version satisfies %s\n" % args.constraint)
                return 1
            print(best)
        elif args.command == "bump":
            print(bump(args.version, args.part, args.preid))
        elif args.command == "check":
            problems = check_lock(parse_lock(_read(args.lockfile)),
                                  parse_constraints_file(_read(args.constraints)))
            for problem in problems:
                print(problem)
            return 1 if problems else 0
        else:
            build_parser().print_usage()
            return 2
    except (VersionError, ConstraintError, LockError) as exc:
        sys.stderr.write("verspec: %s\n" % exc)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
