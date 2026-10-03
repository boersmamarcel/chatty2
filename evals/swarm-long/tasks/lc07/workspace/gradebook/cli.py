"""Command line entry point.

    python3 -m gradebook.cli POLICY ROSTER ASSESSMENTS MARKS [--title TEXT]

Prints the grade report of the section to stdout.
"""

import argparse
import sys

from .grading import GradingError, grade_student
from .loader import LoaderError, load_assessments, load_marks, load_roster
from .policy import PolicyError, parse_policy
from .report import format_report


def _read(path):
    with open(path, encoding="utf-8") as handle:
        return handle.read()


def build_rows(policies, roster, assessments, marks):
    """(name, percentage, letter) for every student on the roster."""
    rows = []
    for student in roster.values():
        pct, letter = grade_student(student.student_id, policies, assessments, marks)
        rows.append((student.name, pct, letter))
    return rows


def main(argv=None):
    parser = argparse.ArgumentParser(prog="gradebook")
    parser.add_argument("policy")
    parser.add_argument("roster")
    parser.add_argument("assessments")
    parser.add_argument("marks")
    parser.add_argument("--title", default="Grades")
    args = parser.parse_args(argv)
    try:
        policies = parse_policy(_read(args.policy))
        roster = load_roster(_read(args.roster))
        assessments = load_assessments(_read(args.assessments))
        marks = load_marks(_read(args.marks), assessments)
        rows = build_rows(policies, roster, assessments, marks)
    except (PolicyError, LoaderError, GradingError) as exc:
        sys.stderr.write("gradebook: %s\n" % exc)
        return 2
    print(format_report(rows, args.title))
    return 0


if __name__ == "__main__":
    sys.exit(main())
