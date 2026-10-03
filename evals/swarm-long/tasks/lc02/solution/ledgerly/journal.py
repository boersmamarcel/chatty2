"""Journal lines and journal entries.

A :class:`JournalLine` posts an ``amount`` to one account on one side
(``"D"`` debit or ``"C"`` credit).  The amount is in the line's transaction
``currency``; ``currency=None`` means the ledger base currency.  Every line
also carries ``base_amount``, the amount in the base currency: for base
currency lines it equals ``amount`` (the ledger fills it in when posting),
for foreign lines it is set by :func:`ledgerly.fx.convert_entry`.

An entry is balanced when its debits equal its credits *in the base
currency*; transaction amounts in different currencies cannot be compared.
"""

from .accounts import CREDIT, DEBIT
from .errors import UnbalancedEntryError, UnknownAccountError, ValidationError
from .money import ZERO, normalize_currency, sum_amounts, to_decimal


class JournalLine(object):
    """One debit or credit of a journal entry."""

    def __init__(self, account, side, amount, currency=None, base_amount=None, memo=""):
        if side not in (DEBIT, CREDIT):
            raise ValidationError("side must be 'D' or 'C', got %r" % (side,))
        self.account = str(account).strip()
        self.side = side
        self.amount = to_decimal(amount)
        self.currency = normalize_currency(currency) if currency else None
        self.base_amount = to_decimal(base_amount) if base_amount is not None else None
        self.memo = memo

    @property
    def is_converted(self):
        """True once ``base_amount`` is known."""
        return self.base_amount is not None

    @property
    def debit(self):
        """Base-currency debit of this line (zero for credit lines)."""
        if self.side == DEBIT and self.base_amount is not None:
            return self.base_amount
        return ZERO

    @property
    def credit(self):
        """Base-currency credit of this line (zero for debit lines)."""
        if self.side == CREDIT and self.base_amount is not None:
            return self.base_amount
        return ZERO

    def copy(self):
        return JournalLine(self.account, self.side, self.amount, self.currency,
                           self.base_amount, self.memo)

    def __repr__(self):
        return "JournalLine(%r, %r, %s %s)" % (
            self.account, self.side, self.amount, self.currency or "base")


class JournalEntry(object):
    """A dated set of lines that must balance.

    ``entry_id`` is assigned by the ledger when the entry is posted.
    """

    def __init__(self, date, description="", lines=None, reference=None):
        self.date = date
        self.description = description
        self.reference = reference
        self.lines = list(lines or [])
        self.entry_id = None

    # -- building -----------------------------------------------------------

    def add(self, line):
        """Append a :class:`JournalLine`; returns the entry for chaining."""
        self.lines.append(line)
        return self

    def debit(self, account, amount, currency=None, memo=""):
        """Append a debit line; returns the entry for chaining."""
        return self.add(JournalLine(account, DEBIT, amount, currency, memo=memo))

    def credit(self, account, amount, currency=None, memo=""):
        """Append a credit line; returns the entry for chaining."""
        return self.add(JournalLine(account, CREDIT, amount, currency, memo=memo))

    # -- inspection ---------------------------------------------------------

    def currencies(self):
        """Sorted transaction currencies used by the lines (None excluded)."""
        return sorted(set(line.currency for line in self.lines if line.currency))

    def accounts(self):
        """Distinct account codes in line order."""
        seen = []
        for line in self.lines:
            if line.account not in seen:
                seen.append(line.account)
        return seen

    def totals(self):
        """Return ``(debit, credit)`` of the transaction amounts.

        Only meaningful when every line uses the same currency.
        """
        debit = sum_amounts(line.amount for line in self.lines if line.side == DEBIT)
        credit = sum_amounts(line.amount for line in self.lines if line.side == CREDIT)
        return debit, credit

    def base_totals(self):
        """Return ``(debit, credit)`` in the ledger base currency."""
        debit = sum_amounts(line.debit for line in self.lines)
        credit = sum_amounts(line.credit for line in self.lines)
        return debit, credit

    def is_balanced(self):
        """True when base debits equal base credits."""
        debit, credit = self.base_totals()
        return debit == credit

    def validate(self, chart):
        """Raise a :class:`ValidationError` subclass if the entry is not postable.

        Checks, in order: at least two lines; every account exists in
        ``chart``; every amount is positive; every line has a base amount;
        base debits equal base credits.
        """
        if len(self.lines) < 2:
            raise ValidationError("entry needs at least two lines")
        for line in self.lines:
            if line.account not in chart:
                raise UnknownAccountError("unknown account %s" % line.account)
            if line.amount <= 0:
                raise ValidationError("line amounts must be positive")
            if line.base_amount is None:
                raise ValidationError("line for %s in %s has not been converted" % (
                    line.account, line.currency))
        debit, credit = self.base_totals()
        if debit != credit:
            raise UnbalancedEntryError("entry is unbalanced (debit %s, credit %s)" % (debit, credit))

    def __repr__(self):
        return "JournalEntry(%s, %r, %d lines)" % (self.date, self.description, len(self.lines))
