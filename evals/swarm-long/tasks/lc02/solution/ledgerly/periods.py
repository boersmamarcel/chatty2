"""Monthly accounting periods, period locking and the period close.

A period is one calendar month, named ``"YYYY-MM"``.  Closing a period moves
the result of its income and expense accounts into retained earnings with one
closing entry and then locks the period: the ledger refuses any later posting
dated inside it.
"""

import calendar as _calendar
import datetime

from .accounts import code_key
from .errors import LedgerError, PeriodLockedError
from .journal import JournalEntry
from .money import ZERO


class Period(object):
    """One calendar month."""

    __slots__ = ("year", "month")

    def __init__(self, year, month):
        if not 1 <= month <= 12:
            raise ValueError("month must be 1..12")
        self.year = year
        self.month = month

    @property
    def start(self):
        """First day of the month."""
        return datetime.date(self.year, self.month, 1)

    @property
    def end(self):
        """Last day of the month."""
        return datetime.date(self.year, self.month, _calendar.monthrange(self.year, self.month)[1])

    @property
    def name(self):
        return "%04d-%02d" % (self.year, self.month)

    def contains(self, day):
        """True when ``day`` falls inside this period."""
        return self.start <= day <= self.end

    def next(self):
        if self.month == 12:
            return Period(self.year + 1, 1)
        return Period(self.year, self.month + 1)

    def previous(self):
        if self.month == 1:
            return Period(self.year - 1, 12)
        return Period(self.year, self.month - 1)

    @classmethod
    def of(cls, day):
        """The period containing ``day``."""
        return cls(day.year, day.month)

    @classmethod
    def parse(cls, text):
        """Parse ``"YYYY-MM"``."""
        try:
            year, month = text.strip().split("-")
            return cls(int(year), int(month))
        except ValueError:
            raise ValueError("invalid period %r" % (text,))

    def __eq__(self, other):
        return isinstance(other, Period) and (self.year, self.month) == (other.year, other.month)

    def __ne__(self, other):
        return not self == other

    def __lt__(self, other):
        return (self.year, self.month) < (other.year, other.month)

    def __hash__(self):
        return hash((self.year, self.month))

    def __repr__(self):
        return "Period(%s)" % self.name


class FiscalCalendar(object):
    """Keeps track of which periods are closed."""

    def __init__(self):
        self._locked = {}

    def lock(self, period):
        self._locked[period.name] = period

    def unlock(self, period):
        self._locked.pop(period.name, None)

    def locked_periods(self):
        """Closed periods in chronological order."""
        return sorted(self._locked.values())

    def locked_period_for(self, day):
        """The closed period containing ``day``, or None."""
        for period in self.locked_periods():
            if period.contains(day):
                return period
        return None

    def is_locked(self, day):
        return self.locked_period_for(day) is not None

    def check_open(self, day):
        """Raise :class:`PeriodLockedError` if ``day`` is in a closed period."""
        period = self.locked_period_for(day)
        if period is not None:
            raise PeriodLockedError("period %s is closed" % period.name)


def period_result(ledger, period):
    """``{code: debit - credit}`` of income/expense lines dated in ``period``."""
    result = {}
    for entry in ledger.entries():
        if not period.contains(entry.date):
            continue
        for line in entry.lines:
            account = ledger.chart.get(line.account)
            if not account.is_pnl:
                continue
            result[account.code] = result.get(account.code, ZERO) + line.debit - line.credit
    return result


def close_period(ledger, period, retained_earnings="3100"):
    """Close ``period``: post the closing entry, then lock the period.

    Returns the posted closing entry.
    """
    calendar = ledger.calendar
    if calendar is None:
        raise LedgerError("ledger has no fiscal calendar")
    calendar.check_open(period.start)
    ledger.chart.get(retained_earnings)
    balances = period_result(ledger, period)
    closing = JournalEntry(period.end, "Close %s" % period.name, reference=period.name)
    to_equity = ZERO
    for code in sorted(balances, key=code_key):
        balance = balances[code]
        if balance == 0:
            continue
        if balance > 0:
            closing.credit(code, balance)
        else:
            closing.debit(code, -balance)
        to_equity += balance
    if to_equity > 0:
        closing.debit(retained_earnings, to_equity)
    elif to_equity < 0:
        closing.credit(retained_earnings, -to_equity)
    if closing.lines:
        ledger.post(closing)
    else:
        closing = None
    calendar.lock(period)
    return closing
