# ledgerly

A small double-entry bookkeeping library (Python 3, standard library only).

* `ledgerly.accounts` - chart of accounts, account types, numeric code ordering
* `ledgerly.journal` - journal lines and entries (debit/credit, base amounts)
* `ledgerly.ledger` - posting and balance queries
* `ledgerly.rates` / `ledgerly.fx` - dated exchange rates and conversion of
  multi-currency entries into the base currency
* `ledgerly.tax` / `ledgerly.invoicing` - VAT codes, invoices and their entries
* `ledgerly.periods` - monthly periods, locking and the period close
* `ledgerly.trial_balance`, `ledgerly.aging`, `ledgerly.formatting` - reports
* `ledgerly.importer` - CSV import of journal entries
* `ledgerly.money` - Decimal helpers; all rounding is half-up to the minor
  units of the currency

Amounts are always `decimal.Decimal`. Dates are `datetime.date`.

Run the tests with:

    python3 -m unittest discover -s tests -t .

Open bugs and feature requests are listed in `ISSUES.md`.
