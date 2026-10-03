"""Category and course percentages, and letter grades.

The course percentage is the weighted mean of the category percentages.
A category in which every assessment is excused for a student is left out
for that student, and the remaining weights are scaled up so that they
still add up to 100 %.
"""

from decimal import Decimal, ROUND_HALF_UP

from .late import apply_late
from .models import EXCUSED
from .policy import drop_lowest

# Lower bound (inclusive) of each letter grade, best first.
SCALE = [
    (93.0, "A"),
    (90.0, "A-"),
    (87.0, "B+"),
    (83.0, "B"),
    (80.0, "B-"),
    (77.0, "C+"),
    (73.0, "C"),
    (70.0, "C-"),
    (60.0, "D"),
]


class GradingError(ValueError):
    """Raised when a grade cannot be computed."""


def round_half_up(value, places):
    """Round a float half-up (away from zero on .5) to `places` decimals."""
    quantum = Decimal(1).scaleb(-places)
    return float(Decimal(str(value)).quantize(quantum, rounding=ROUND_HALF_UP))


def entries_for(student_id, category, assessments, marks):
    """(Assessment, points) pairs of one student in one category.

    Late penalties are applied, missing work counts as 0 and excused work
    is left out. An assessment without any mark for the student counts as
    missing.
    """
    by_key = {}
    for mark in marks:
        if mark.student_id == student_id:
            by_key[mark.assessment_key] = mark
    entries = []
    for assessment in assessments.values():
        if assessment.category != category:
            continue
        mark = by_key.get(assessment.key)
        if mark is None:
            entries.append((assessment, 0.0))
            continue
        if mark.points is EXCUSED:
            continue
        points = apply_late(mark.points, assessment.max_points, assessment.due,
                            mark.submitted, mark.extension_days)
        entries.append((assessment, 0.0 if points is None else float(points)))
    return entries


def category_percent(entries):
    """Earned points over possible points, in percent (None if no entries)."""
    if not entries:
        return None
    earned = sum(points for _, points in entries)
    possible = sum(assessment.max_points for assessment, _ in entries)
    return 100.0 * earned / possible


def course_percent(student_id, policies, assessments, marks):
    """Weighted course percentage of one student (unrounded float)."""
    total = 0.0
    weight_used = 0.0
    for policy in policies.values():
        entries = entries_for(student_id, policy.name, assessments, marks)
        entries = drop_lowest(entries, policy.drop)
        pct = category_percent(entries)
        if pct is None:
            continue
        total += pct * policy.weight
        weight_used += policy.weight
    if weight_used == 0:
        raise GradingError("no graded work for student %r" % (student_id,))
    return total / weight_used


def letter_for(pct):
    """Letter grade of a course percentage (see SCALE)."""
    if pct < 0:
        raise GradingError("negative percentage: %r" % (pct,))
    rounded = round_half_up(pct, 1)
    for threshold, letter in SCALE:
        if rounded >= threshold:
            return letter
    return "F"


def grade_student(student_id, policies, assessments, marks):
    """(percentage rounded half-up to 2 decimals, letter) of one student."""
    pct = course_percent(student_id, policies, assessments, marks)
    return round_half_up(pct, 2), letter_for(pct)
