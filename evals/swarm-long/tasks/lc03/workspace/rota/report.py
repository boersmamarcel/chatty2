"""Plain-text reports over an assigned rota."""

from datetime import timedelta

from .timeutil import format_minutes, start_of_day


def minutes_in_window(shift, lo, hi):
    """Minutes of ``shift`` that fall inside ``[lo, hi)``."""
    part = shift.interval.clip(lo, hi)
    return part.minutes if part is not None else 0


def weekly_hours(shifts, employees, week_start):
    """Hours worked per employee in the week starting on the date ``week_start``.

    Returns a list of ``(employee id, hours)`` pairs.
    """
    lo = start_of_day(week_start)
    hi = lo + timedelta(days=7)
    totals = {}
    for shift in shifts:
        if shift.employee is None:
            continue
        minutes = minutes_in_window(shift, lo, hi)
        if minutes:
            totals[shift.employee] = totals.get(shift.employee, 0) + minutes
    return [(emp, round(minutes / 60.0, 1)) for emp, minutes in sorted(totals.items())]


def format_hours_table(rows, employees):
    """Render ``weekly_hours`` output as an aligned text table.

    ``employees`` is a list of :class:`rota.models.Employee` used for names.
    """
    names = dict((e.emp_id, e.name) for e in employees)
    lines = []
    width = max([len(names.get(emp, emp)) for emp, _ in rows] + [8])
    lines.append("%-6s  %-*s  %6s" % ("ID", width, "Employee", "Hours"))
    for emp, hours in rows:
        lines.append("%-6s  %-*s  %6s" % (emp, width, names.get(emp, emp), hours))
    return "\n".join(lines)


def daily_coverage(shifts, day, roles):
    """Count of shifts per role starting on ``day``; unknown roles are ignored.

    Returns ``{role: (assigned, total)}`` for every role in ``roles``.
    """
    out = dict((role, (0, 0)) for role in roles)
    for shift in shifts:
        if shift.start.date() != day or shift.role not in out:
            continue
        assigned, total = out[shift.role]
        out[shift.role] = (assigned + (1 if shift.employee else 0), total + 1)
    return out


def unfilled_report(shifts):
    """One line per unassigned shift, chronological: ``S1 2026-03-02 06:00 nurse (8h00m)``."""
    lines = []
    for shift in sorted(shifts, key=lambda s: (s.start, s.shift_id)):
        if shift.employee is not None:
            continue
        lines.append("%s %s %s (%s)" % (shift.shift_id, shift.start.strftime("%Y-%m-%d %H:%M"),
                                        shift.role, format_minutes(shift.minutes)))
    return lines


def conflict_report(conflicts):
    """Conflicts grouped by employee, one indented message per line."""
    lines = []
    current = None
    for c in sorted(conflicts, key=lambda c: c.key()):
        if c.employee != current:
            current = c.employee
            lines.append("%s:" % current)
        lines.append("  [%s] %s" % (c.kind, c.message))
    return lines
