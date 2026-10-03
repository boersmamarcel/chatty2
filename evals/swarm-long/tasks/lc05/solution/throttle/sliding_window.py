"""Sliding window log limiter.

The limiter keeps the timestamp of every request it allowed during the last
``window`` seconds. A request is allowed when fewer than ``limit`` such
timestamps exist. Compared with a fixed window counter this never lets
``2 * limit`` requests through around a window boundary, at the price of
storing one timestamp per allowed request.

Window semantics: at time ``t`` the window is the half-open interval
``(t - window, t]``. A request made exactly ``window`` seconds ago is outside
the window and no longer counts.
"""

import collections

from .plans import Rate


class SlidingWindowLog(object):
    """Sliding window log driven by an injectable clock.

    :param limit: requests allowed per window (positive int).
    :param window: window length in seconds (positive number).
    :param clock: object with ``now()``.
    """

    def __init__(self, limit, window, clock):
        if int(limit) <= 0:
            raise ValueError("limit must be positive")
        if window <= 0:
            raise ValueError("window must be positive")
        self.limit = int(limit)
        self.window = window
        self.clock = clock
        self._log = collections.deque()

    @classmethod
    def for_rate(cls, rate, clock):
        """Window limiter enforcing a :class:`~throttle.plans.Rate`."""
        if not isinstance(rate, Rate):
            raise TypeError("expected a Rate, got %r" % (rate,))
        return cls(rate.limit, rate.period, clock)

    # -- internals -----------------------------------------------------------

    def _evict(self, now):
        """Drop timestamps that have left the window ending at ``now``."""
        horizon = now - self.window
        while self._log and self._log[0] <= horizon:
            self._log.popleft()

    def _check_cost(self, cost):
        if int(cost) != cost or cost <= 0:
            raise ValueError("cost must be a positive integer")

    # -- queries -------------------------------------------------------------

    def count(self):
        """Number of requests currently inside the window."""
        self._evict(self.clock.now())
        return len(self._log)

    def remaining(self):
        """Requests that would still be allowed right now."""
        return max(0, self.limit - self.count())

    def can_acquire(self, cost=1):
        """True when ``cost`` more requests fit in the window right now."""
        self._check_cost(cost)
        return self.count() + cost <= self.limit

    def retry_after(self, cost=1):
        """Seconds until ``cost`` more requests fit (0.0 when they already do).

        That is the moment the ``cost``-th entry, counted from the oldest
        entry that would have to leave, drops out of the window.
        """
        self._check_cost(cost)
        now = self.clock.now()
        self._evict(now)
        excess = len(self._log) + cost - self.limit
        if excess <= 0:
            return 0.0
        if cost > self.limit:
            raise ValueError("cost %s exceeds window limit %s" % (cost, self.limit))
        leaving = self._log[excess - 1]
        return max(0.0, leaving + self.window - now)

    def reset_after(self):
        """Seconds until the window is empty again."""
        now = self.clock.now()
        self._evict(now)
        if not self._log:
            return 0.0
        return max(0.0, self._log[-1] + self.window - now)

    # -- mutation ------------------------------------------------------------

    def try_acquire(self, cost=1):
        """Record ``cost`` requests at the current time if they fit.

        Returns True when they were recorded.
        """
        self._check_cost(cost)
        now = self.clock.now()
        self._evict(now)
        if len(self._log) + cost > self.limit:
            return False
        for _ in range(int(cost)):
            self._log.append(now)
        return True

    def __repr__(self):
        return "SlidingWindowLog(limit=%r, window=%r, entries=%d)" % (
            self.limit, self.window, len(self._log))
