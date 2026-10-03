"""Rates and plans.

A :class:`Rate` is "``limit`` requests per ``period`` seconds". A
:class:`Plan` bundles one or more rates (all of which must allow a request),
the algorithm used to enforce them, an optional burst size and an optional
monthly quota.

Rates are usually written as short strings in plan configuration files and
parsed with :func:`parse_rate`::

    >>> parse_rate("100/min")
    Rate(100, 60)
    >>> parse_rate("5000/day")
    Rate(5000, 86400)
"""

import re

from .errors import ConfigError

#: Seconds per unit name. Keys are lowercase.
UNIT_SECONDS = {
    "s": 1, "sec": 1, "secs": 1, "second": 1, "seconds": 1,
    "m": 60, "min": 60, "mins": 60, "minute": 60, "minutes": 60,
    "h": 3600, "hr": 3600, "hour": 3600, "hours": 3600,
    "d": 86400, "day": 86400, "days": 86400,
}

#: Algorithms a plan may name.
ALGORITHMS = ("token_bucket", "sliding_window", "fixed_window")

_SLASH_FORM = re.compile(r"^(\d+)\s*/\s*(\d*)\s*([a-z]+)$")


class Rate(object):
    """``limit`` requests per ``period`` seconds."""

    __slots__ = ("limit", "period")

    def __init__(self, limit, period):
        limit = int(limit)
        period = int(period)
        if limit <= 0:
            raise ConfigError("rate limit must be positive")
        if period <= 0:
            raise ConfigError("rate period must be positive")
        self.limit = limit
        self.period = period

    @property
    def per_second(self):
        """Sustained throughput in requests per second (float)."""
        return float(self.limit) / self.period

    def describe(self):
        """A compact human readable form, e.g. ``"100/min"`` or ``"7/90s"``."""
        for name, seconds in (("day", 86400), ("hour", 3600), ("min", 60), ("s", 1)):
            if self.period == seconds:
                return "%d/%s" % (self.limit, name)
        return "%d/%ds" % (self.limit, self.period)

    def __eq__(self, other):
        if not isinstance(other, Rate):
            return NotImplemented
        return (self.limit, self.period) == (other.limit, other.period)

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result

    def __hash__(self):
        return hash((self.limit, self.period))

    def __repr__(self):
        return "Rate(%d, %d)" % (self.limit, self.period)


def parse_rate(text):
    """Parse a rate string into a :class:`Rate`.

    Accepted form: ``"<count>/<unit>"`` or ``"<count>/<multiplier><unit>"``,
    e.g. ``"100/min"``, ``"10/s"``, ``"30/5m"`` (30 per five minutes).
    Unit names are listed in :data:`UNIT_SECONDS`.

    Raises :class:`ConfigError` with a single message:

    * ``invalid rate '<text>'`` when the text does not have that shape,
    * ``unknown unit '<unit>'`` for an unrecognised unit,
    * ``rate limit must be positive`` / ``rate period must be positive``.
    """
    if not isinstance(text, str):
        raise ConfigError("invalid rate %r" % (text,))
    cleaned = text.strip()
    match = _SLASH_FORM.match(cleaned)
    if match is None:
        raise ConfigError("invalid rate '%s'" % cleaned)
    count, multiplier, unit = match.groups()
    if unit not in UNIT_SECONDS:
        raise ConfigError("unknown unit '%s'" % unit)
    multiplier = int(multiplier) if multiplier else 1
    if multiplier <= 0:
        raise ConfigError("rate period must be positive")
    return Rate(int(count), multiplier * UNIT_SECONDS[unit])


class Plan(object):
    """A named set of limits.

    :param name: plan name, e.g. ``"free"``.
    :param rates: list of :class:`Rate`; a request must fit every one.
    :param algorithm: one of :data:`ALGORITHMS`.
    :param burst: token bucket capacity override (only meaningful for
        ``token_bucket``); ``None`` means "same as the rate's limit".
    :param quota: requests allowed per billing period, or ``None``.
    :param anchor_day: day of month on which billing periods start (1-31).
    """

    def __init__(self, name, rates, algorithm="token_bucket", burst=None, quota=None, anchor_day=1):
        if not name:
            raise ConfigError("plan name must not be empty")
        if algorithm not in ALGORITHMS:
            raise ConfigError("unknown algorithm '%s'" % algorithm)
        if burst is not None and int(burst) <= 0:
            raise ConfigError("burst must be positive")
        if quota is not None and int(quota) < 0:
            raise ConfigError("quota must not be negative")
        if not 1 <= int(anchor_day) <= 31:
            raise ConfigError("anchor day must be between 1 and 31")
        self.name = name
        self.rates = list(rates)
        self.algorithm = algorithm
        self.burst = None if burst is None else int(burst)
        self.quota = None if quota is None else int(quota)
        self.anchor_day = int(anchor_day)

    def strictest(self):
        """The rate with the lowest sustained throughput (first one on ties)."""
        best = None
        for rate in self.rates:
            if best is None or rate.per_second < best.per_second:
                best = rate
        return best

    def describe(self):
        """One-line summary, e.g. ``"free: 10/s, 1000/hour (token_bucket)"``."""
        rates = ", ".join(r.describe() for r in self.rates) or "unlimited"
        return "%s: %s (%s)" % (self.name, rates, self.algorithm)

    def __repr__(self):
        return "Plan(%r, %r, algorithm=%r, burst=%r, quota=%r, anchor_day=%r)" % (
            self.name, self.rates, self.algorithm, self.burst, self.quota, self.anchor_day)
