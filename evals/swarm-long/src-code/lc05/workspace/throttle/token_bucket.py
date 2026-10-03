"""Token bucket limiter.

A bucket holds up to ``capacity`` tokens and gains ``refill_rate`` tokens per
second. A request of cost ``c`` is allowed when at least ``c`` tokens are in
the bucket, and then removes them. Tokens are fractional: after half a second
at 1 token/s the bucket has gained 0.5 tokens.

Refilling is lazy: the bucket remembers when it was last updated and adds the
tokens earned since then whenever it is consulted.
"""

from .plans import Rate


class TokenBucket(object):
    """A token bucket driven by an injectable clock.

    :param capacity: maximum number of tokens (positive number).
    :param refill_rate: tokens added per second (positive number).
    :param clock: object with ``now()``.
    :param initial: tokens at creation; defaults to ``capacity`` (full).
    """

    def __init__(self, capacity, refill_rate, clock, initial=None):
        if capacity <= 0:
            raise ValueError("capacity must be positive")
        if refill_rate <= 0:
            raise ValueError("refill rate must be positive")
        self.capacity = capacity
        self.refill_rate = float(refill_rate)
        self.clock = clock
        self._tokens = float(capacity if initial is None else initial)
        if not 0 <= self._tokens <= capacity:
            raise ValueError("initial tokens must be between 0 and capacity")
        self._updated = clock.now()

    @classmethod
    def for_rate(cls, rate, clock, burst=None):
        """Bucket enforcing a :class:`~throttle.plans.Rate`.

        The capacity is ``burst`` when given, otherwise ``rate.limit``; the
        refill rate is ``rate.limit / rate.period`` tokens per second.
        """
        if not isinstance(rate, Rate):
            raise TypeError("expected a Rate, got %r" % (rate,))
        capacity = burst if burst is not None else rate.limit
        return cls(capacity, rate.per_second, clock)

    # -- internals -----------------------------------------------------------

    def _refill(self):
        now = self.clock.now()
        elapsed = now - self._updated
        if elapsed > 0:
            self._tokens = self._tokens + elapsed * self.refill_rate
            self._updated = now

    def _check_cost(self, cost):
        if cost <= 0:
            raise ValueError("cost must be positive")

    # -- queries -------------------------------------------------------------

    def available(self):
        """Tokens currently in the bucket (float)."""
        self._refill()
        return self._tokens

    def remaining(self):
        """Alias of :meth:`available`, part of the common limiter interface."""
        return self.available()

    def can_acquire(self, cost=1):
        """True when a request of ``cost`` would be allowed right now."""
        self._check_cost(cost)
        return self.available() >= cost

    def time_until(self, cost=1):
        """Seconds until ``cost`` tokens are available (0.0 when they already are)."""
        self._check_cost(cost)
        missing = cost - self.available()
        if missing <= 0:
            return 0.0
        return missing / self.refill_rate

    def retry_after(self, cost=1):
        """Common limiter interface: same as :meth:`time_until`."""
        return self.time_until(cost)

    def reset_after(self):
        """Seconds until the bucket is full again."""
        missing = self.capacity - self.available()
        if missing <= 0:
            return 0.0
        return missing / self.refill_rate

    # -- mutation ------------------------------------------------------------

    def try_acquire(self, cost=1):
        """Take ``cost`` tokens if available. Returns True when taken.

        A refused request leaves the bucket unchanged.
        """
        self._check_cost(cost)
        self._refill()
        if self._tokens >= cost:
            self._tokens -= cost
            return True
        return False

    def __repr__(self):
        return "TokenBucket(capacity=%r, refill_rate=%r, tokens=%r)" % (
            self.capacity, self.refill_rate, self._tokens)
