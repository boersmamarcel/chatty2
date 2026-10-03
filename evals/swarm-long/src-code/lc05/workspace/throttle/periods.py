"""Billing periods.

Quotas are counted per billing period. A tenant's billing periods start on a
fixed *anchor day* of the month at 00:00 UTC and run until the same anchor day
of the next month (exclusive). For anchor days that a month does not have
(29-31), the period starts on the last day of that month instead: with anchor
day 31 the periods start on Jan 31, Feb 28 (Feb 29 in leap years), Mar 31,
Apr 30, and so on.

All datetimes handled here are timezone-aware UTC datetimes; timestamps are
seconds since the epoch as returned by a clock's ``now()``.
"""

import calendar
import datetime

UTC = datetime.timezone.utc


def from_timestamp(ts):
    """Timezone-aware UTC datetime for epoch seconds ``ts``."""
    return datetime.datetime.fromtimestamp(ts, UTC)


def to_timestamp(dt):
    """Epoch seconds for an aware datetime."""
    return dt.timestamp()


def shift_month(year, month, delta):
    """``(year, month)`` moved by ``delta`` months (may be negative)."""
    index = year * 12 + (month - 1) + delta
    return index // 12, index % 12 + 1


def anchor_date(year, month, anchor_day):
    """Start of the billing period that begins in ``year``-``month``."""
    first = datetime.datetime(year, month, 1, tzinfo=UTC)
    return first + datetime.timedelta(days=anchor_day - 1)


class BillingPeriod(object):
    """The interval ``[start, end)`` of one billing period."""

    __slots__ = ("start", "end")

    def __init__(self, start, end):
        if end <= start:
            raise ValueError("billing period must end after it starts")
        self.start = start
        self.end = end

    def contains(self, moment):
        """True when the aware datetime ``moment`` falls inside the period."""
        return self.start <= moment <= self.end

    def seconds_left(self, moment):
        """Seconds from ``moment`` until the period ends (never negative)."""
        return max(0.0, (self.end - moment).total_seconds())

    def label(self):
        """``"YYYY-MM-DD..YYYY-MM-DD"`` (start date .. end date)."""
        return "%s..%s" % (self.start.date().isoformat(), self.end.date().isoformat())

    def __eq__(self, other):
        if not isinstance(other, BillingPeriod):
            return NotImplemented
        return (self.start, self.end) == (other.start, other.end)

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result

    def __hash__(self):
        return hash((self.start, self.end))

    def __repr__(self):
        return "BillingPeriod(%s, %s)" % (self.start.isoformat(), self.end.isoformat())


def period_starting_in(year, month, anchor_day):
    """The billing period whose start lies in ``year``-``month``."""
    start = anchor_date(year, month, anchor_day)
    next_year, next_month = shift_month(year, month, 1)
    return BillingPeriod(start, anchor_date(next_year, next_month, anchor_day))


def billing_period(anchor_day, ts):
    """The billing period containing the epoch timestamp ``ts``."""
    if not 1 <= anchor_day <= 31:
        raise ValueError("anchor day must be between 1 and 31")
    moment = from_timestamp(ts)
    for delta in (-1, 0, 1):
        year, month = shift_month(moment.year, moment.month, delta)
        period = period_starting_in(year, month, anchor_day)
        if period.contains(moment):
            return period
    raise AssertionError("no billing period contains %s" % moment.isoformat())


def periods_between(anchor_day, start_ts, end_ts):
    """Every billing period overlapping ``[start_ts, end_ts)``, in order."""
    if end_ts <= start_ts:
        return []
    period = billing_period(anchor_day, start_ts)
    end = from_timestamp(end_ts)
    found = []
    while period.start < end:
        found.append(period)
        year, month = period.end.year, period.end.month
        period = period_starting_in(year, month, anchor_day)
    return found
