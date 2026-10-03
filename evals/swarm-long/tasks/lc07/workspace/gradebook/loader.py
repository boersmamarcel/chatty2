"""CSV import of the three gradebook files.

roster.csv        student_id,name,section
assessments.csv   key,category,max_points,due
marks.csv         student_id,assessment,points,submitted,extension_days

Cells are stripped of surrounding whitespace. `due` and `submitted` are
'YYYY-MM-DD' (meaning 23:59 that day) or 'YYYY-MM-DD HH:MM'; both may be
empty. `extension_days` may be empty (no extension).
"""

import collections
import csv
import io

from .models import Assessment, Mark, Student, parse_datetime

ROSTER_COLUMNS = ("student_id", "name")
ASSESSMENT_COLUMNS = ("key", "category", "max_points")
MARK_COLUMNS = ("student_id", "assessment", "points")


class LoaderError(ValueError):
    """Raised for a malformed input file; the message names the line."""


def _rows(text, required, what):
    """Data rows of a CSV text as dicts with stripped keys and values."""
    reader = csv.DictReader(io.StringIO(text))
    header = [name.strip().lower() for name in (reader.fieldnames or [])]
    missing = [name for name in required if name not in header]
    if missing:
        raise LoaderError("%s: missing column(s) %s" % (what, ", ".join(missing)))
    reader.fieldnames = header
    rows = []
    for row in reader:
        rows.append({key: (value or "").strip() for key, value in row.items() if key is not None})
    return rows


def load_roster(text):
    """OrderedDict student_id -> Student."""
    students = collections.OrderedDict()
    for number, row in enumerate(_rows(text, ROSTER_COLUMNS, "roster"), 2):
        sid = row["student_id"]
        if not sid:
            raise LoaderError("roster line %d: empty student_id" % number)
        if sid in students:
            raise LoaderError("roster line %d: duplicate student_id %r" % (number, sid))
        students[sid] = Student(sid, row["name"], row.get("section", ""))
    return students


def load_assessments(text):
    """OrderedDict key -> Assessment, in file order."""
    assessments = collections.OrderedDict()
    for number, row in enumerate(_rows(text, ASSESSMENT_COLUMNS, "assessments"), 2):
        key = row["key"]
        if key in assessments:
            raise LoaderError("assessments line %d: duplicate key %r" % (number, key))
        try:
            max_points = float(row["max_points"])
            due = parse_datetime(row["due"]) if row.get("due") else None
            assessments[key] = Assessment(key, row["category"].lower(), max_points, due)
        except ValueError as exc:
            raise LoaderError("assessments line %d: %s" % (number, exc))
    return assessments


def parse_points(text):
    """Points of one marks cell (a float)."""
    return float(text)


def load_marks(text, assessments=None):
    """List of Mark, in file order.

    With `assessments` given, a mark for an unknown assessment is an error.
    """
    marks = []
    for number, row in enumerate(_rows(text, MARK_COLUMNS, "marks"), 1):
        try:
            points = parse_points(row["points"])
            submitted = parse_datetime(row["submitted"]) if row.get("submitted") else None
            extension = int(row.get("extension_days") or 0)
        except ValueError as exc:
            raise LoaderError("marks line %d: %s" % (number, exc))
        key = row["assessment"]
        if assessments is not None and key not in assessments:
            raise LoaderError("marks line %d: unknown assessment %r" % (number, key))
        marks.append(Mark(row["student_id"], key, points, submitted, extension))
    return marks
