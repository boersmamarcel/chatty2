"""Injectable clocks.

Every time-dependent component of ``throttle`` takes a clock object instead of
reading the system time itself. A clock is anything with a ``now()`` method
returning seconds since the Unix epoch as a float (UTC). Production code wires
in a clock backed by the host's time source; tests use :class:`ManualClock`,
which only moves when told to, so behaviour is fully deterministic.
"""

from .errors import ThrottleError


class Clock(object):
    """Interface: ``now()`` returns seconds since the epoch (UTC) as a float."""

    def now(self):
        raise NotImplementedError("Clock subclasses must implement now()")


class ManualClock(Clock):
    """A clock that only advances when the caller says so.

    >>> clock = ManualClock(100.0)
    >>> clock.advance(2.5)
    102.5
    >>> clock.now()
    102.5

    Time never moves backwards: :meth:`advance` rejects negative steps and
    :meth:`set` rejects a timestamp earlier than the current one.
    """

    def __init__(self, start=0.0):
        if start < 0:
            raise ThrottleError("clock start must not be negative: %r" % (start,))
        self._now = float(start)

    def now(self):
        return self._now

    def advance(self, seconds):
        """Move the clock forward by ``seconds`` and return the new time."""
        if seconds < 0:
            raise ThrottleError("cannot advance a clock by a negative amount: %r" % (seconds,))
        self._now += seconds
        return self._now

    def set(self, timestamp):
        """Jump to ``timestamp`` (must not be in the past) and return it."""
        if timestamp < self._now:
            raise ThrottleError("clock cannot go backwards (%r < %r)" % (timestamp, self._now))
        self._now = float(timestamp)
        return self._now

    def __repr__(self):
        return "ManualClock(%r)" % (self._now,)


class OffsetClock(Clock):
    """A view of another clock shifted by a fixed offset in seconds.

    Useful for simulating a node whose clock is skewed relative to the rest of
    a cluster. The offset may be negative.
    """

    def __init__(self, base, offset):
        self._base = base
        self.offset = float(offset)

    def now(self):
        return self._base.now() + self.offset

    def __repr__(self):
        return "OffsetClock(%r, %r)" % (self._base, self.offset)
