"""ledgerly: a small double-entry bookkeeping ledger.

The package keeps a chart of accounts, posts balanced journal entries into a
ledger, converts foreign-currency postings through a rate table, computes VAT
on invoices, closes monthly periods into retained earnings and produces trial
balance and receivables aging reports.

All amounts are :class:`decimal.Decimal`; floats are only accepted at the
edges and converted immediately (see :mod:`ledgerly.money`).
"""

from .accounts import Account, ChartOfAccounts, default_chart
from .journal import JournalEntry, JournalLine
from .ledger import Ledger
from .rates import RateTable

__all__ = [
    "Account",
    "ChartOfAccounts",
    "default_chart",
    "JournalEntry",
    "JournalLine",
    "Ledger",
    "RateTable",
]

__version__ = "0.4.2"
