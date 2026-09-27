from collections import deque


class RateLimiter:
    """At most max_calls allowed calls in any window of period seconds."""

    def __init__(self, max_calls, period, clock):
        self.max_calls = max_calls
        self.period = period
        self.clock = clock
        self.calls = deque()

    def _expire(self, now):
        while self.calls and self.calls[0] + self.period <= now:
            self.calls.popleft()

    def remaining(self):
        self._expire(self.clock())
        return self.max_calls - len(self.calls)

    def allow(self):
        now = self.clock()
        self._expire(now)
        if len(self.calls) >= self.max_calls:
            return False
        self.calls.append(now)
        return True
