"""Exchange-rate table.

A rate is quoted as *units of the base currency per one unit of the foreign
currency*: with base EUR, ``add("USD", date(2024, 1, 1), "0.92")`` means
1 USD = 0.92 EUR from 1 January 2024 onwards.  A rate stays in effect until a
later rate for the same currency takes over.
"""

import bisect
from decimal import Decimal

from .errors import RateNotFound
from .money import normalize_currency, quantize, to_decimal


class RateTable(object):
    """Dated exchange rates against one base currency."""

    def __init__(self, base="EUR"):
        self.base = normalize_currency(base)
        self._dates = {}
        self._rates = {}

    def add(self, currency, effective, rate):
        """Record ``rate`` for ``currency`` effective from ``effective``.

        Adding a second rate for the same currency and date replaces the
        first one.
        """
        rate = to_decimal(rate)
        if rate <= 0:
            raise ValueError("rates must be positive")
        dates = self._dates.setdefault(currency, [])
        rates = self._rates.setdefault(currency, [])
        index = bisect.bisect_left(dates, effective)
        if index < len(dates) and dates[index] == effective:
            rates[index] = rate
        else:
            dates.insert(index, effective)
            rates.insert(index, rate)

    def rate_on(self, currency, day):
        """The rate of ``currency`` in effect on ``day``.

        The base currency always has rate 1.
        """
        if currency == self.base:
            return Decimal(1)
        dates = self._dates.get(currency)
        if not dates:
            raise RateNotFound("rate not found for %s" % currency)
        index = bisect.bisect_left(dates, day)
        if index == 0:
            raise RateNotFound("rate not found for %s" % currency)
        return self._rates[currency][index - 1]

    def convert(self, amount, currency, day):
        """``amount`` of ``currency`` in the base currency, rounded half-up."""
        return quantize(to_decimal(amount) * self.rate_on(currency, day), self.base)

    def currencies(self):
        """Currencies with at least one rate, sorted."""
        return sorted(code for code, dates in self._dates.items() if dates)

    def history(self, currency):
        """``[(effective, rate), ...]`` of ``currency`` in date order."""
        return list(zip(self._dates.get(currency, []), self._rates.get(currency, [])))

    @classmethod
    def from_rows(cls, rows, base="EUR"):
        """Build a table from ``(currency, effective, rate)`` tuples."""
        table = cls(base)
        for currency, effective, rate in rows:
            table.add(currency, effective, rate)
        return table
