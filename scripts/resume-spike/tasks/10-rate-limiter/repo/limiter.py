class RateLimiter:
    """At most max_calls allowed calls in any window of period seconds."""

    def __init__(self, max_calls, period, clock):
        raise NotImplementedError

    def allow(self):
        raise NotImplementedError
