"""Core domain objects: employees, shifts and meetings."""

from datetime import timedelta

from .intervals import Interval
from .timeutil import check_comparable, minutes_between


class Employee(object):
    """A person who can be put on the rota.

    :param emp_id: unique identifier, e.g. ``"E07"``; compared as a string.
    :param name: display name.
    :param skills: roles the employee may work (``{"nurse", "triage"}``).
    :param max_minutes_per_week: cap on rostered minutes per ISO week.
    :param time_off: list of :class:`Interval` during which the employee is
        not available (leave, training, ...).
    """

    def __init__(self, emp_id, name, skills=(), max_minutes_per_week=2400, time_off=()):
        if not emp_id:
            raise ValueError("employee id must not be empty")
        self.emp_id = emp_id
        self.name = name
        self.skills = frozenset(skills)
        self.max_minutes_per_week = max_minutes_per_week
        self.time_off = list(time_off)

    def can_work(self, role):
        """True when the employee has the skill for ``role``."""
        return role in self.skills

    def is_available(self, start, end):
        """True when ``[start, end)`` does not collide with any time off."""
        check_comparable(start, end)
        for off in self.time_off:
            if off.start < end and start < off.end:
                return False
        return True

    def add_time_off(self, start, end):
        """Record a period of leave."""
        self.time_off.append(Interval(start, end))

    def __repr__(self):
        return "Employee(%r, %r)" % (self.emp_id, self.name)


class Shift(object):
    """A block of work for one role, optionally assigned to an employee.

    Shifts may run past midnight (night shifts): only ``end > start`` is
    required.
    """

    def __init__(self, shift_id, start, end, role, location="", employee=None):
        self.interval = Interval(start, end)
        self.shift_id = shift_id
        self.role = role
        self.location = location
        self.employee = employee

    @property
    def start(self):
        return self.interval.start

    @property
    def end(self):
        return self.interval.end

    @property
    def minutes(self):
        """Length of the shift in minutes."""
        return self.interval.minutes

    def is_overnight(self):
        """True when the shift ends on a later calendar day than it starts."""
        next_midnight = self.start.replace(hour=0, minute=0, second=0, microsecond=0) \
            + timedelta(days=1)
        return self.end > next_midnight

    def assigned_to(self, emp_id):
        """A copy of the shift assigned to ``emp_id``."""
        return Shift(self.shift_id, self.start, self.end, self.role, self.location, emp_id)

    def __repr__(self):
        return "Shift(%r, %s-%s, %r, employee=%r)" % (
            self.shift_id, self.start.isoformat(), self.end.isoformat(), self.role, self.employee)


class Meeting(object):
    """A meeting with attendees; ``rrule`` is an optional recurrence text."""

    def __init__(self, uid, title, start, end, attendees=(), location="",
                 description="", rrule=None):
        check_comparable(start, end)
        if end <= start:
            raise ValueError("meeting must end after it starts")
        self.uid = uid
        self.title = title
        self.start = start
        self.end = end
        self.attendees = list(attendees)
        self.location = location
        self.description = description
        self.rrule = rrule

    @property
    def minutes(self):
        return minutes_between(self.start, self.end)

    @property
    def interval(self):
        return Interval(self.start, self.end)

    def __repr__(self):
        return "Meeting(%r, %r, %s)" % (self.uid, self.title, self.start.isoformat())
