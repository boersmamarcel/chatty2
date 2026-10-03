"""Conversion of multi-currency journal entries into the base currency.

Each foreign line is converted with the rate in effect on the entry date.
Converting line by line and rounding each result can leave the entry a cent
or two out of balance in the base currency; a small difference is booked to a
rounding account, a larger one is an error.
"""

from decimal import Decimal

from .accounts import CREDIT, DEBIT
from .errors import UnbalancedEntryError
from .journal import JournalLine
from .money import minor_units, quantize

#: Allowed rounding difference per converted foreign line.
TOLERANCE_PER_LINE = Decimal("0.01")


def convert_amount(amount, rate, base):
    """``amount * rate`` rounded to the minor units of ``base``."""
    return Decimal(str(round(float(amount) * float(rate), minor_units(base))))


def convert_entry(entry, rates, rounding_account=None):
    """Fill in ``base_amount`` of every line of ``entry`` and balance it.

    Returns the entry (modified in place).  See ISSUES.md / the docs for the
    rounding-difference rules.
    """
    base = rates.base
    foreign_lines = 0
    for line in entry.lines:
        if line.currency in (None, base):
            line.base_amount = line.amount
            continue
        rate = rates.rate_on(line.currency, entry.date)
        line.base_amount = convert_amount(line.amount, rate, base)
        foreign_lines += 1
    debit, credit = entry.base_totals()
    difference = debit - credit
    if difference == 0:
        return entry
    limit = TOLERANCE_PER_LINE * foreign_lines
    if rounding_account is None or abs(difference) >= limit:
        raise UnbalancedEntryError("entry unbalanced by %s %s after conversion" % (
            quantize(difference, base), base))
    side = CREDIT if difference > 0 else DEBIT
    entry.add(JournalLine(rounding_account, side, abs(difference), currency=None,
                          base_amount=abs(difference), memo="FX rounding"))
    return entry


def revalue(balance_foreign, booked_base, rate, base):
    """Unrealised FX result of an open foreign balance at ``rate``.

    Positive means a gain in the base currency.
    """
    return quantize(Decimal(balance_foreign) * rate, base) - booked_base
