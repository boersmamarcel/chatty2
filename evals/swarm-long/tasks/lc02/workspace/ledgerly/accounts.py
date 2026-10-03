"""Chart of accounts.

An account has a numeric code (a string of 3 to 6 digits, e.g. ``"1200"``),
a name and one of five types.  Asset and expense accounts are *debit-normal*
(a debit increases them); liability, equity and income accounts are
*credit-normal*.

Account codes are compared as integers wherever an order is needed, so
``"900"`` sorts before ``"1000"``; :func:`code_key` is the sort key to use.
"""

from .errors import LedgerError, UnknownAccountError
from .money import ZERO, to_decimal

ASSET = "asset"
LIABILITY = "liability"
EQUITY = "equity"
INCOME = "income"
EXPENSE = "expense"

ACCOUNT_TYPES = (ASSET, LIABILITY, EQUITY, INCOME, EXPENSE)
DEBIT_NORMAL = frozenset([ASSET, EXPENSE])
PROFIT_AND_LOSS = frozenset([INCOME, EXPENSE])

DEBIT = "D"
CREDIT = "C"


def code_key(code):
    """Sort key for account codes: numeric value, then the text itself."""
    return (int(code), str(code))


def validate_code(code):
    """Return ``code`` as a stripped string or raise ``LedgerError``."""
    text = str(code).strip()
    if not text.isdigit() or not 3 <= len(text) <= 6:
        raise LedgerError("invalid account code %r" % (code,))
    return text


class Account(object):
    """One account of the chart."""

    __slots__ = ("code", "name", "type", "parent", "currency")

    def __init__(self, code, name, type, parent=None, currency=None):
        if type not in ACCOUNT_TYPES:
            raise LedgerError("invalid account type %r" % (type,))
        self.code = validate_code(code)
        self.name = name
        self.type = type
        self.parent = validate_code(parent) if parent is not None else None
        self.currency = currency

    @property
    def normal_side(self):
        """``DEBIT`` for assets and expenses, ``CREDIT`` otherwise."""
        return DEBIT if self.type in DEBIT_NORMAL else CREDIT

    @property
    def is_pnl(self):
        """True for income and expense accounts (closed at period end)."""
        return self.type in PROFIT_AND_LOSS

    def signed(self, debit, credit):
        """Balance on the normal side: positive when the account is 'normal'."""
        debit = to_decimal(debit)
        credit = to_decimal(credit)
        if self.normal_side == DEBIT:
            return debit - credit
        return credit - debit

    def __repr__(self):
        return "Account(%r, %r, %r)" % (self.code, self.name, self.type)


class ChartOfAccounts(object):
    """A collection of accounts keyed by code.

    Iteration yields accounts ordered by :func:`code_key`.
    """

    def __init__(self, accounts=()):
        self._accounts = {}
        for account in accounts:
            self.add(account)

    def add(self, account):
        """Add an :class:`Account`; duplicate codes are rejected."""
        if account.code in self._accounts:
            raise LedgerError("duplicate account code %s" % account.code)
        if account.parent is not None and account.parent not in self._accounts:
            raise UnknownAccountError("unknown account %s" % account.parent)
        self._accounts[account.code] = account
        return account

    def create(self, code, name, type, parent=None, currency=None):
        """Shortcut for ``add(Account(...))``."""
        return self.add(Account(code, name, type, parent=parent, currency=currency))

    def get(self, code):
        """Return the account with ``code`` or raise ``UnknownAccountError``."""
        key = str(code).strip()
        try:
            return self._accounts[key]
        except KeyError:
            raise UnknownAccountError("unknown account %s" % key)

    def __contains__(self, code):
        return str(code).strip() in self._accounts

    def __iter__(self):
        for code in sorted(self._accounts, key=code_key):
            yield self._accounts[code]

    def __len__(self):
        return len(self._accounts)

    def codes(self):
        """All codes ordered by :func:`code_key`."""
        return [account.code for account in self]

    def of_type(self, *types):
        """Accounts whose type is one of ``types``, in code order."""
        return [account for account in self if account.type in types]

    def children(self, code):
        """Direct children of the account ``code``, in code order."""
        self.get(code)
        return [account for account in self if account.parent == code]

    def descendants(self, code):
        """Every account below ``code`` (depth first, code order)."""
        found = []
        for child in self.children(code):
            found.append(child)
            found.extend(self.descendants(child.code))
        return found

    def rollup(self, code, balances):
        """Sum ``balances`` ({code: Decimal}) over ``code`` and its descendants."""
        total = balances.get(code, ZERO)
        for child in self.descendants(code):
            total += balances.get(child.code, ZERO)
        return total


def default_chart():
    """A small standard chart used by examples and tests."""
    chart = ChartOfAccounts()
    rows = [
        ("1000", "Cash", ASSET),
        ("1100", "Bank", ASSET),
        ("1200", "Accounts receivable", ASSET),
        ("1300", "Input VAT", ASSET),
        ("1500", "Equipment", ASSET),
        ("2000", "Accounts payable", LIABILITY),
        ("2100", "Output VAT", LIABILITY),
        ("3000", "Share capital", EQUITY),
        ("3100", "Retained earnings", EQUITY),
        ("4000", "Sales", INCOME),
        ("4100", "Service revenue", INCOME),
        ("5000", "Cost of goods sold", EXPENSE),
        ("6000", "Rent", EXPENSE),
        ("6100", "Salaries", EXPENSE),
        ("6900", "FX rounding", EXPENSE),
        ("7000", "FX gains and losses", EXPENSE),
    ]
    for code, name, type_ in rows:
        chart.create(code, name, type_)
    return chart
