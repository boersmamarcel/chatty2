"""Fixed window counter limiter.

Time is cut into consecutive windows of ``window`` seconds aligned on the
epoch: window ``k`` covers ``[k * window, (k + 1) * window)``. Each window has
its own counter; a request is allowed while the counter of the current window
is below ``limit``. The counter starts from zero in every new window.

Cheap (one integer per key) but bursty: up to ``2 * limit`` requests can pass
within a short span straddling a window boundary.
"""

import math

from .plans import Rate


class FixedWindowCounter(object):
    """Fixed window counter driven by an injectable clock."""

    def __init__(self, limit, window, clock):
        if int(limit) <= 0:
            raise ValueError("limit must be positive")
        if window <= 0:
            raise ValueError("window must be positive")
        self.limit = int(limit)
        self.window = window
        self.clock = clock
        self._index = None
        self._count = 0

    @classmethod
    def for_rate(cls, rate, clock):
        """Counter enforcing a :class:`~throttle.plans.Rate`."""
        if not isinstance(rate, Rate):
            raise TypeError("expected a Rate, got %r" % (rate,))
        return cls(rate.limit, rate.period, clock)

    def _current(self):
        """Roll the counter over when the clock entered a new window."""
        index = int(math.floor(self.clock.now() / self.window))
        if index != self._index:
            self._index = index
            self._count = 0
        return index

    def _check_cost(self, cost):
        if int(cost) != cost or cost <= 0:
            raise ValueError("cost must be a positive integer")

    def count(self):
        """Requests counted in the current window."""
        self._current()
        return self._count

    def remaining(self):
        """Requests still allowed in the current window."""
        return max(0, self.limit - self.count())

    def can_acquire(self, cost=1):
        """True when ``cost`` more requests fit in the current window."""
        self._check_cost(cost)
        return self.count() + cost <= self.limit

    def window_start(self):
        """Start timestamp of the current window."""
        return self._current() * self.window

    def reset_after(self):
        """Seconds until the next window starts."""
        index = self._current()
        return (index + 1) * self.window - self.clock.now()

    def retry_after(self, cost=1):
        """Seconds until ``cost`` requests fit (0.0 when they already do)."""
        if self.can_acquire(cost):
            return 0.0
        if cost > self.limit:
            raise ValueError("cost %s exceeds window limit %s" % (cost, self.limit))
        return self.reset_after()

    def try_acquire(self, cost=1):
        """Count ``cost`` requests if they fit; True when counted."""
        if not self.can_acquire(cost):
            return False
        self._count += int(cost)
        return True

    def __repr__(self):
        return "FixedWindowCounter(limit=%r, window=%r, count=%r)" % (
            self.limit, self.window, self._count)
