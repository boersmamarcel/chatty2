"""The general ledger: posted entries and account balances.

The ledger validates entries against its chart of accounts, refuses postings
into closed periods (when it has a :class:`ledgerly.periods.FiscalCalendar`)
and answers balance queries.  Date filters are inclusive on both ends.
"""

from .accounts import CREDIT, DEBIT, code_key
from .errors import LedgerError
from .money import ZERO, normalize_currency


class Ledger(object):
    """Posted journal entries plus the chart they post to."""

    def __init__(self, chart, base_currency="EUR", calendar=None):
        self.chart = chart
        self.base_currency = normalize_currency(base_currency)
        self.calendar = calendar
        self._entries = []
        self._seq = 0

    # -- posting ------------------------------------------------------------

    def post(self, entry):
        """Validate and store ``entry``; returns it with ``entry_id`` set.

        Base-currency lines (``currency`` None or equal to the base currency)
        get ``base_amount = amount``.  Foreign lines must already have been
        converted (see :func:`ledgerly.fx.convert_entry`).
        """
        if entry.entry_id is not None:
            raise LedgerError("entry %s is already posted" % entry.entry_id)
        for line in entry.lines:
            if line.base_amount is None and line.currency in (None, self.base_currency):
                line.base_amount = line.amount
        entry.validate(self.chart)
        if self.calendar is not None:
            self.calendar.check_open(entry.date)
        self._seq += 1
        entry.entry_id = "JE%05d" % self._seq
        self._entries.append(entry)
        return entry

    def post_many(self, entries):
        """Post several entries in order; returns them."""
        return [self.post(entry) for entry in entries]

    def post_foreign(self, entry, rates, rounding_account=None):
        """Convert a multi-currency entry with ``rates`` and post it."""
        from .fx import convert_entry
        if rates.base != self.base_currency:
            raise LedgerError("rate table base %s differs from ledger base %s" % (
                rates.base, self.base_currency))
        convert_entry(entry, rates, rounding_account=rounding_account)
        return self.post(entry)

    # -- queries ------------------------------------------------------------

    def entries(self, start=None, end=None):
        """Posted entries dated within ``[start, end]``, by date then posting order."""
        selected = [
            entry for entry in self._entries
            if (start is None or entry.date >= start) and (end is None or entry.date <= end)
        ]
        return sorted(selected, key=lambda e: (e.date, e.entry_id))

    def lines(self, start=None, end=None):
        """``(entry, line)`` pairs of every posted line in the date range."""
        for entry in self.entries(start, end):
            for line in entry.lines:
                yield entry, line

    def account_totals(self, as_of=None, start=None):
        """``{code: (debit, credit)}`` in base currency for accounts with lines."""
        totals = {}
        for _entry, line in self.lines(start, as_of):
            debit, credit = totals.get(line.account, (ZERO, ZERO))
            if line.side == DEBIT:
                debit += line.base_amount
            else:
                credit += line.base_amount
            totals[line.account] = (debit, credit)
        return totals

    def balance(self, code, as_of=None, start=None):
        """Balance of ``code`` on its normal side (see :meth:`Account.signed`)."""
        account = self.chart.get(code)
        debit, credit = self.account_totals(as_of=as_of, start=start).get(account.code, (ZERO, ZERO))
        return account.signed(debit, credit)

    def balances(self, as_of=None, start=None):
        """``{code: signed balance}`` for every account of the chart."""
        totals = self.account_totals(as_of=as_of, start=start)
        result = {}
        for account in self.chart:
            debit, credit = totals.get(account.code, (ZERO, ZERO))
            result[account.code] = account.signed(debit, credit)
        return result

    def activity(self, code, start=None, end=None):
        """Lines posted to ``code`` with a running balance.

        Returns a list of ``(date, entry_id, debit, credit, running)`` tuples.
        """
        account = self.chart.get(code)
        running = ZERO
        rows = []
        for entry, line in self.lines(start, end):
            if line.account != account.code:
                continue
            running += account.signed(line.debit, line.credit)
            rows.append((entry.date, entry.entry_id, line.debit, line.credit, running))
        return rows

    def profit(self, start=None, end=None):
        """Income minus expenses over the date range."""
        totals = self.account_totals(as_of=end, start=start)
        result = ZERO
        for code in sorted(totals, key=code_key):
            account = self.chart.get(code)
            if not account.is_pnl:
                continue
            debit, credit = totals[code]
            result += credit - debit
        return result

    def __len__(self):
        return len(self._entries)


__all__ = ["Ledger", "DEBIT", "CREDIT"]
