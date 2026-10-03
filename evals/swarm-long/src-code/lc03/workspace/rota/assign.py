"""Greedy automatic rota assignment.

Shifts are filled in chronological order (ties by shift id).  For each shift
the eligible employees are those who

* have the skill for the shift's role,
* are available (no time off colliding with the shift),
* have no other shift in this run overlapping it,
* get at least ``min_rest_hours`` of rest before and after it relative to
  their other shifts in this run, and
* stay within their weekly minute cap (ISO week of the shift start).

Among the eligible employees the one with the fewest minutes assigned so far
in this run wins; remaining ties go to the smallest employee id.
"""

from datetime import timedelta

from .errors import AssignmentError
from .timeutil import week_start


class AssignmentResult(object):
    """Outcome of :func:`assign_rota`.

    ``assignments`` maps shift id to employee id, ``unfilled`` lists the ids
    of shifts nobody could take (in fill order), ``minutes`` maps employee id
    to total assigned minutes.
    """

    def __init__(self):
        self.assignments = {}
        self.unfilled = []
        self.minutes = {}

    def shifts_of(self, emp_id):
        """Sorted shift ids assigned to ``emp_id``."""
        return sorted(sid for sid, emp in self.assignments.items() if emp == emp_id)

    def apply(self, shifts):
        """Copies of ``shifts`` with their assigned employee filled in."""
        return [s.assigned_to(self.assignments[s.shift_id]) if s.shift_id in self.assignments
                else s for s in shifts]

    def __repr__(self):
        return "AssignmentResult(%d assigned, %d unfilled)" % (
            len(self.assignments), len(self.unfilled))


def _week_minutes(booked, monday):
    return sum(s.minutes for s in booked if week_start(s.start.date()) == monday)


def _rest_ok(booked, shift, min_rest):
    gap = timedelta(hours=min_rest)
    for other in booked:
        if other.interval.overlaps(shift.interval):
            return False
        if other.end <= shift.start and shift.start - other.end < gap:
            return False
        if shift.end <= other.start and other.start - shift.end < gap:
            return False
    return True


def _eligible(employee, shift, booked, min_rest):
    if not employee.can_work(shift.role):
        return False
    if not _rest_ok(booked, shift, min_rest):
        return False
    monday = week_start(shift.start.date())
    if _week_minutes(booked, monday) + shift.minutes > employee.max_minutes_per_week:
        return False
    return True


def assign_rota(shifts, employees, min_rest_hours=11):
    """Assign every shift to an employee; see the module docstring.

    Returns an :class:`AssignmentResult`.  Raises :class:`AssignmentError`
    on duplicate shift or employee ids.
    """
    ids = [s.shift_id for s in shifts]
    if len(set(ids)) != len(ids):
        raise AssignmentError("duplicate shift ids")
    emp_ids = [e.emp_id for e in employees]
    if len(set(emp_ids)) != len(emp_ids):
        raise AssignmentError("duplicate employee ids")
    result = AssignmentResult()
    booked = dict((e.emp_id, []) for e in employees)
    result.minutes = dict((e.emp_id, 0) for e in employees)
    for shift in sorted(shifts, key=lambda s: (s.start, s.shift_id)):
        candidates = [e for e in employees
                      if _eligible(e, shift, booked[e.emp_id], min_rest_hours)]
        if not candidates:
            result.unfilled.append(shift.shift_id)
            continue
        candidates.sort(key=lambda e: (len(booked[e.emp_id]), e.name))
        chosen = candidates[0]
        booked[chosen.emp_id].append(shift)
        result.minutes[chosen.emp_id] += shift.minutes
        result.assignments[shift.shift_id] = chosen.emp_id
    return result


def coverage_ratio(result, shifts):
    """Fraction of shifts filled, as a float in ``[0, 1]`` (1.0 when empty)."""
    if not shifts:
        return 1.0
    return float(len(result.assignments)) / len(shifts)
