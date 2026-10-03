"""Decision counters.

:class:`Metrics` counts allowed and denied decisions per tenant and keeps the
``Retry-After`` waits handed out, so operators can see who is being throttled
and for how long. :mod:`throttle.report` renders these numbers as a table.
"""

import math
from decimal import ROUND_HALF_UP, Decimal


def deny_percent(allowed, denied):
    """Share of denied decisions in percent, rounded to one decimal place.

    Returns ``0.0`` when there were no decisions at all.
    """
    total = allowed + denied
    if total == 0:
        return Decimal("0.0")
    exact = Decimal(denied * 100) / Decimal(total)
    return exact.quantize(Decimal("0.1"), rounding=ROUND_HALF_UP)


def percentile(values, pct):
    """Nearest-rank percentile of ``values`` (``pct`` in 0..100).

    Returns ``None`` for an empty sequence. The nearest-rank method picks the
    smallest value such that at least ``pct`` percent of the values are less
    than or equal to it; ``pct == 0`` gives the minimum.
    """
    if not values:
        return None
    if not 0 <= pct <= 100:
        raise ValueError("percentile must be between 0 and 100")
    ordered = sorted(values)
    if pct == 0:
        return ordered[0]
    rank = int(math.ceil(pct / 100.0 * len(ordered)))
    return ordered[rank - 1]


class TenantStats(object):
    """Counters of one tenant."""

    __slots__ = ("tenant", "allowed", "denied")

    def __init__(self, tenant, allowed=0, denied=0):
        self.tenant = tenant
        self.allowed = allowed
        self.denied = denied

    @property
    def total(self):
        return self.allowed + self.denied

    def __repr__(self):
        return "TenantStats(%r, allowed=%d, denied=%d)" % (self.tenant, self.allowed, self.denied)


class Metrics(object):
    """In-memory decision counters."""

    def __init__(self):
        self._stats = {}
        self._waits = []

    def record(self, tenant, allowed, retry_after=None):
        """Count one decision for ``tenant``.

        ``retry_after`` (seconds) is remembered for denied decisions.
        """
        stats = self._stats.get(tenant)
        if stats is None:
            stats = self._stats[tenant] = TenantStats(tenant)
        if allowed:
            stats.allowed += 1
        else:
            stats.denied += 1
            if retry_after is not None:
                self._waits.append(retry_after)

    def stats(self, tenant):
        """:class:`TenantStats` of ``tenant`` (zeros when never seen)."""
        found = self._stats.get(tenant)
        if found is None:
            return TenantStats(tenant)
        return TenantStats(tenant, found.allowed, found.denied)

    def tenants(self):
        """Every tenant with at least one decision, sorted by name."""
        return sorted(self._stats)

    def totals(self):
        """Sum over all tenants, as a :class:`TenantStats` named ``"TOTAL"``."""
        allowed = sum(s.allowed for s in self._stats.values())
        denied = sum(s.denied for s in self._stats.values())
        return TenantStats("TOTAL", allowed, denied)

    def deny_rate(self, tenant):
        """Denied share of ``tenant`` in percent (see :func:`deny_percent`)."""
        stats = self.stats(tenant)
        return deny_percent(stats.allowed, stats.denied)

    def wait_percentile(self, pct):
        """Nearest-rank percentile of the recorded waits, or ``None``."""
        return percentile(self._waits, pct)

    def reset(self):
        """Forget every counter."""
        self._stats.clear()
        del self._waits[:]
