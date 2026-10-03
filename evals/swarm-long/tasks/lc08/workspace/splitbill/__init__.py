"""splitbill: share group expenses and settle up.

Modules:

    money     -- amounts in integer cents: parsing, formatting, splitting
    currency  -- exchange rates and conversion to the group currency
    expenses  -- the Expense record and the split methods
    importer  -- CSV import of an expense list
    balances  -- net balance per person
    settle    -- the transfers that settle all balances
    cli       -- command line entry point and plain-text summaries
"""

__version__ = "1.2.0"
