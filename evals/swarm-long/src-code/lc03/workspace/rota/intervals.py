"""Half-open time intervals.

An :class:`Interval` covers ``[start, end)``: the start instant belongs to the
interval, the end instant does not.  Two intervals that merely touch (one ends
exactly when the other starts) therefore do *not* overlap.
"""

from datetime import timedelta

from .timeutil import check_comparable, minutes_between


class Interval(object):
    """A half-open span of time ``[start, end)`` with ``end > start``."""

    __slots__ = ("start", "end")

    def __init__(self, start, end):
        check_comparable(start, end)
        if end <= start:
            raise ValueError("interval end %r must be after start %r" % (end, start))
        self.start = start
        self.end = end

    @property
    def duration(self):
        """Length as a :class:`datetime.timedelta`."""
        return self.end - self.start

    @property
    def minutes(self):
        """Length in whole minutes."""
        return minutes_between(self.start, self.end)

    def overlaps(self, other):
        """True when the two intervals share at least one instant."""
        return self.start < other.end and other.start < self.end

    def touches(self, other):
        """True when the intervals are adjacent (one ends where the other starts)."""
        return self.end == other.start or other.end == self.start

    def contains(self, instant):
        """True when ``instant`` lies in ``[start, end)``."""
        return self.start <= instant < self.end

    def covers(self, other):
        """True when ``other`` lies completely inside this interval."""
        return self.start <= other.start and other.end <= self.end

    def intersection(self, other):
        """The common part of two intervals, or ``None``."""
        if not self.overlaps(other):
            return None
        return Interval(max(self.start, other.start), min(self.end, other.end))

    def clip(self, lo, hi):
        """The part of this interval inside ``[lo, hi)``, or ``None``."""
        if self.end <= lo or self.start >= hi:
            return None
        return Interval(max(self.start, lo), self.end)

    def shifted(self, delta):
        """A copy moved by ``delta`` (a timedelta)."""
        return Interval(self.start + delta, self.end + delta)

    def split_at(self, instant):
        """Split into the parts before and after ``instant``.

        Returns a list with one or two intervals.
        """
        if not self.start < instant < self.end:
            return [self]
        return [Interval(self.start, instant), Interval(instant, self.end)]

    def _key(self):
        return (self.start, self.end)

    def __eq__(self, other):
        return isinstance(other, Interval) and self._key() == other._key()

    def __ne__(self, other):
        return not self == other

    def __lt__(self, other):
        return self._key() < other._key()

    def __hash__(self):
        return hash(self._key())

    def __repr__(self):
        return "Interval(%r, %r)" % (self.start, self.end)


def merge_intervals(intervals):
    """Sort and merge overlapping or touching intervals.

    ``[9-10, 10-11, 12-13]`` becomes ``[9-11, 12-13]``.  The input is not
    modified; a new sorted list is returned.
    """
    ordered = sorted(intervals)
    merged = []
    for iv in ordered:
        if merged and iv.start <= merged[-1].end:
            if iv.end > merged[-1].end:
                merged[-1] = Interval(merged[-1].start, iv.end)
        else:
            merged.append(Interval(iv.start, iv.end))
    return merged


def subtract(intervals, cut):
    """Remove ``cut`` from every interval of a list; returns a new sorted list."""
    out = []
    for iv in sorted(intervals):
        if not iv.overlaps(cut):
            out.append(iv)
            continue
        if iv.start < cut.start:
            out.append(Interval(iv.start, cut.start))
        if cut.end < iv.end:
            out.append(Interval(cut.end, iv.end))
    return out


def total_minutes(intervals):
    """Minutes covered by a list of intervals, counting overlaps once."""
    return sum(iv.minutes for iv in merge_intervals(intervals))


def gaps(intervals, lo, hi):
    """The free parts of ``[lo, hi)`` not covered by ``intervals``."""
    free = [Interval(lo, hi)]
    for iv in merge_intervals(intervals):
        free = subtract(free, iv)
    return free


def span(intervals):
    """The smallest interval covering all given intervals (``None`` if empty)."""
    if not intervals:
        return None
    return Interval(min(iv.start for iv in intervals),
                    max(iv.end for iv in intervals))


def daily_slices(interval):
    """Split an interval at every midnight it crosses.

    A night shift ``Mon 22:00 - Tue 06:00`` gives ``[Mon 22:00 - Tue 00:00,
    Tue 00:00 - Tue 06:00]``.
    """
    pieces = []
    current = interval.start
    while current < interval.end:
        midnight = (current + timedelta(days=1)).replace(
            hour=0, minute=0, second=0, microsecond=0)
        end = min(midnight, interval.end)
        pieces.append(Interval(current, end))
        current = end
    return pieces
