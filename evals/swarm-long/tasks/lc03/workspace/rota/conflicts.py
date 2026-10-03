"""Conflict detection on an assigned rota.

Three kinds of conflict are reported:

``overlap``
    one employee has two shifts that share time;
``rest``
    the rest between the end of one shift and the start of the employee's
    next shift is shorter than the minimum rest (default 11 hours);
``hours``
    an employee is rostered for more minutes in one ISO week (by shift start)
    than their ``max_minutes_per_week``.

Unassigned shifts (``employee is None``) are ignored.
"""

from .timeutil import format_minutes, minutes_between, week_start


class Conflict(object):
    """One detected problem; ``first``/``second`` are shift ids (or ``None``)."""

    def __init__(self, kind, employee, first, second, message):
        self.kind = kind
        self.employee = employee
        self.first = first
        self.second = second
        self.message = message

    def key(self):
        return (self.employee, self.kind, self.first or "", self.second or "")

    def __eq__(self, other):
        return isinstance(other, Conflict) and self.key() == other.key() \
            and self.message == other.message

    def __ne__(self, other):
        return not self == other

    def __repr__(self):
        return "Conflict(%r)" % self.message


def _by_employee(shifts):
    groups = {}
    for shift in shifts:
        if shift.employee is None:
            continue
        groups.setdefault(shift.employee, []).append(shift)
    return groups


def find_overlaps(shifts):
    """``overlap`` conflicts, one per overlapping pair of an employee's shifts."""
    out = []
    groups = _by_employee(shifts)
    for emp in sorted(groups):
        items = sorted(groups[emp], key=lambda s: (s.start, s.shift_id))
        for i, a in enumerate(items):
            for b in items[i + 1:]:
                if b.start >= a.end:
                    break
                out.append(Conflict("overlap", emp, a.shift_id, b.shift_id,
                                    "%s: %s overlaps %s" % (emp, a.shift_id, b.shift_id)))
    return out


def rest_violations(shifts, min_rest_hours=11):
    """``rest`` conflicts between consecutive shifts of each employee.

    The rest is measured from the end of a shift to the start of the
    employee's next shift.  Overlapping pairs are left to
    :func:`find_overlaps`.  Message format::

        E1: only 8h00m rest between S1 and S2 (minimum 11h00m)
    """
    limit = int(min_rest_hours * 60)
    out = []
    groups = _by_employee(shifts)
    for emp in sorted(groups):
        items = groups[emp]
        for a, b in zip(items, items[1:]):
            if b.start < a.end:
                continue
            gap = minutes_between(a.start, b.start)
            if gap <= limit:
                out.append(Conflict(
                    "rest", emp, a.shift_id, b.shift_id,
                    "%s: only %s rest between %s and %s (minimum %s)" % (
                        emp, format_minutes(gap), a.shift_id, b.shift_id,
                        format_minutes(limit))))
    return out


def hours_violations(shifts, employees):
    """``hours`` conflicts: weekly minutes above an employee's cap.

    ``employees`` maps employee id to :class:`rota.models.Employee`; shifts of
    unknown employees are not checked.
    """
    totals = {}
    for shift in shifts:
        if shift.employee is None or shift.employee not in employees:
            continue
        key = (shift.employee, week_start(shift.start.date()))
        totals[key] = totals.get(key, 0) + shift.minutes
    out = []
    for (emp, monday) in sorted(totals):
        cap = employees[emp].max_minutes_per_week
        if totals[(emp, monday)] > cap:
            out.append(Conflict("hours", emp, None, None,
                                "%s: %s rostered in week of %s (cap %s)" % (
                                    emp, format_minutes(totals[(emp, monday)]),
                                    monday.isoformat(), format_minutes(cap))))
    return out


def find_conflicts(shifts, employees=None, min_rest_hours=11):
    """All conflicts, sorted by employee, kind, first and second shift id."""
    found = find_overlaps(shifts) + rest_violations(shifts, min_rest_hours)
    if employees:
        found += hours_violations(shifts, employees)
    return sorted(found, key=lambda c: c.key())


def conflict_summary(conflicts):
    """``{kind: count}`` over a list of conflicts."""
    summary = {}
    for c in conflicts:
        summary[c.kind] = summary.get(c.kind, 0) + 1
    return summary
