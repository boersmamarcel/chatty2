"""Availability windows and meeting slot search.

Each person has a list of availability windows (:class:`rota.intervals.Interval`).
A person's windows may be entered in any order and may overlap or touch; they
describe the union of the time the person is free.  :func:`find_slot` looks
for the earliest time at which *every* person is free for a whole meeting.
"""

from datetime import datetime, timedelta

from .intervals import Interval, subtract
from .timeutil import daterange


class Availability(object):
    """Availability windows per person."""

    def __init__(self):
        self._windows = {}

    def add(self, person, start, end):
        """Add one window ``[start, end)`` for ``person``."""
        self._windows.setdefault(person, []).append(Interval(start, end))

    def add_weekly(self, person, weekday, start_time, end_time, from_date, to_date):
        """Add a window on every ``weekday`` (0=Monday) in ``[from_date, to_date)``."""
        for day in daterange(from_date, to_date):
            if day.weekday() == weekday:
                self.add(person, datetime.combine(day, start_time),
                         datetime.combine(day, end_time))

    def block(self, person, start, end):
        """Remove ``[start, end)`` from the person's windows (e.g. a booked meeting)."""
        self._windows[person] = subtract(self._windows.get(person, []), Interval(start, end))

    def windows(self, person):
        """The person's windows sorted by start (as entered, not merged)."""
        return sorted(self._windows.get(person, []))

    def people(self):
        """Sorted list of people with at least one window."""
        return sorted(p for p, ws in self._windows.items() if ws)

    def as_dict(self, people=None):
        """``{person: windows}`` for the given people (default: everybody)."""
        people = self.people() if people is None else people
        return dict((p, self.windows(p)) for p in people)


def intersect_windows(a, b):
    """Intersection of two sorted lists of non-overlapping intervals."""
    out = []
    i = j = 0
    while i < len(a) and j < len(b):
        common = a[i].intersection(b[j])
        if common is not None:
            out.append(common)
        if a[i].end < b[j].end:
            i += 1
        else:
            j += 1
    return out


def common_windows(windows_by_person):
    """Time when everybody in ``windows_by_person`` is free, sorted."""
    lists = [sorted(ws) for _, ws in sorted(windows_by_person.items())]
    if not lists:
        return []
    common = lists[0]
    for other in lists[1:]:
        common = intersect_windows(common, other)
    return common


def _align_up(value, granularity):
    """Round ``value`` up to the next multiple of ``granularity`` minutes after midnight."""
    midnight = value.replace(hour=0, minute=0, second=0, microsecond=0)
    elapsed = value - midnight
    step = timedelta(minutes=granularity)
    remainder = elapsed % step
    if remainder:
        value = value + (step - remainder)
    return value


def find_slot(windows_by_person, duration_minutes, earliest=None, latest=None,
              granularity=15):
    """Earliest slot of ``duration_minutes`` where everybody is free.

    The slot starts at a multiple of ``granularity`` minutes after midnight,
    not before ``earliest`` and ends no later than ``latest`` (both optional).
    Returns an :class:`Interval` or ``None``.
    """
    if duration_minutes <= 0:
        raise ValueError("duration must be positive")
    if granularity <= 0:
        raise ValueError("granularity must be positive")
    if not windows_by_person:
        return None
    duration = timedelta(minutes=duration_minutes)
    for window in common_windows(windows_by_person):
        start = window.start
        if earliest is not None and start < earliest:
            start = earliest
        start = _align_up(start, granularity)
        end = window.end
        if latest is not None and latest < end:
            end = latest
        if start + duration < end:
            return Interval(start, start + duration)
    return None


def free_minutes(windows_by_person):
    """Total minutes during which everybody is free."""
    return sum(iv.minutes for iv in common_windows(windows_by_person))
