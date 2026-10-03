# Open issues

Each issue below is independent of the others. Where an issue gives an
"Acceptance" list, every bullet is required. Names, signatures, messages and
formats are exact. Existing behaviour that an issue does not mention must keep
working.

---

## Issue 1: exchange rate effective on the posting day is ignored

`RateTable.rate_on` (module `ledgerly/rates.py`) skips the rate that becomes
effective on the requested day and uses the previous one; a USD payment
booked on 1 February with a rate effective 1 February is converted at the
January rate. Users also report that rates added as `"usd"` cannot be found
as `"USD"`, and the error message is unhelpful.

Acceptance:

- `RateTable.rate_on(currency, day)` returns the rate with the latest
  effective date **on or before** `day` (a rate effective on `day` applies on
  `day`). `RateTable.convert` follows.
- Currency codes are normalised with `ledgerly.money.normalize_currency`
  (surrounding whitespace stripped, upper-cased) both when adding a rate and
  when looking one up: after `add(" usd ", date(2024, 1, 1), "0.9")`,
  `rate_on("Usd", date(2024, 1, 1))` returns `Decimal("0.9")`, and
  `rate_on("eur", day)` on a table with base `EUR` returns `Decimal("1")`.
- When no rate applies (the currency has no rates at all, or all its rates
  become effective after `day`), `rate_on` raises `RateNotFound` with the
  message exactly `no USD rate on or before 2024-01-01` (normalised currency
  code, ISO date).

Tests: `tests/test_rates.py`.

---

## Issue 2: invoice VAT is off by a cent

Customers recompute our VAT and get a different figure. Two separate
causes were found, in `ledgerly/tax.py` and `ledgerly/invoicing.py`.

Acceptance:

- `tax.compute_tax(net, rate, currency=None)` computes `net * rate` in
  `Decimal` and rounds **half-up** to the minor units of `currency` (2 decimals
  by default, 0 for JPY), returning a `Decimal` with exactly that many
  decimals. Examples: `compute_tax(Decimal("0.50"), Decimal("0.09"))` is
  `Decimal("0.05")`; `compute_tax(Decimal("1050"), Decimal("0.21"), "JPY")` is
  `Decimal("221")`; `str(compute_tax(Decimal("10"), Decimal("0.10")))` is
  `"1.00"`.
- `Invoice.tax_breakdown(table)` computes the tax **once per tax code**, on the
  sum of the net amounts of the lines with that code (each line's net is
  still `quantity * unit_price` rounded half-up), instead of rounding the tax
  of every line and adding the results. Example: three lines of quantity 1
  at 0.50 with code `R` (9 %) give `[("R", Decimal("1.50"), Decimal("0.14"))]`.
  The result stays sorted by code; `tax_total`, `total` and `build_entry` use
  these figures.
- `tax.split_gross(gross, rate, currency=None)` splits a tax-inclusive amount
  so that the parts always add up: `tax = gross * rate / (1 + rate)` rounded
  half-up, `net = gross - tax`. Example:
  `split_gross(Decimal("10.00"), Decimal("0.21"))` is
  `(Decimal("8.26"), Decimal("1.74"))`.

---

## Issue 3: trial balance clutter and unreadable amounts

The trial balance (`ledgerly/trial_balance.py`) lists accounts whose activity
nets to zero, ignores `include_zero`, and orders `"900"` after `"4000"`.
Large amounts in the rendered report are printed as `12500.00` and are hard to
read.

Acceptance:

- `trial_balance.build(ledger, as_of=None, include_zero=False)` returns one row
  per account whose net balance (debits minus credits up to and including
  `as_of`) is not zero. Accounts with a zero net balance are left out.
- With `include_zero=True` every account of the chart appears, including
  accounts that were never posted to (both columns zero).
- Rows are ordered by account code compared as numbers (`"900"` before
  `"1000"`), i.e. the order of iterating the chart of accounts.
- `formatting.format_amount(amount, currency=None)` groups thousands with
  commas: `format_amount(Decimal("1234567.5"))` is `"1,234,567.50"`,
  `format_amount(Decimal("-1234.5"))` is `"-1,234.50"`,
  `format_amount(Decimal("999.995"))` is `"1,000.00"`,
  `format_amount(Decimal("1234.5"), "JPY")` is `"1,235"`, and
  `format_amount(Decimal("0"))` is `"0.00"`. `trial_balance.render` therefore
  prints amounts (including the `TOTAL` row) like `12,500.00`.

Tests: `tests/test_trial_balance.py`.

---

## Issue 4: period close misses the last day and fails on zero balances

`periods.close_period(ledger, period, retained_earnings="3100")` in
`ledgerly/periods.py` has several problems: entries dated on the last day of
the month are not closed, they can still be posted after the close, the
closing entry is dated on the first day of the month, and the close crashes
with `ValidationError` when an income or expense account nets to zero (or
when there is nothing to close).

Acceptance:

- `Period.contains(day)` is inclusive at both ends:
  `Period(2024, 2).contains(date(2024, 2, 29))` is `True`.
- The closing entry is dated on the **last day** of the period, its
  description is `"Close YYYY-MM"` (e.g. `"Close 2024-03"`), and it zeroes the
  activity of every income and expense account dated within the period (both
  ends inclusive): one line per account with a non-zero net, ordered by
  account code as numbers; an account with a debit net is credited, one with a
  credit net is debited. The last line goes to `retained_earnings`: a credit
  for a profit, a debit for a loss, and no line when the result is exactly
  zero.
- Accounts whose net for the period is zero get no line. If no line remains
  at all, nothing is posted and `close_period` returns `None`; the period is
  still locked. Otherwise it returns the posted closing entry.
- After the close, `Ledger.post` of any entry dated within the period,
  including its last day, raises `PeriodLockedError` with the message
  `period 2024-03 is closed`; entries dated in the following period still
  post. Closing the same period again raises the same error.

---

## Issue 5: receivables aging puts invoices in the wrong bucket

The aging report (`ledgerly/aging.py`) counts days from the invoice date,
treats the 30th, 60th and 90th day overdue as belonging to the next bucket,
and applies payments in the wrong order.

Acceptance:

- `days_overdue(item, as_of)` is `(as_of - item.due_date).days`.
- `bucket_for(days)`: `0` or less is `"current"`, 1 to 30 is `"1-30"`, 31 to 60
  is `"31-60"`, 61 to 90 is `"61-90"`, 91 or more is `"90+"`.
- `allocate_payments(items, payments, as_of=None)` only applies payments dated
  on or before `as_of` (all payments when `as_of` is `None`); a payment dated
  after the report date does not reduce anything. Each customer's payments
  are applied to that customer's items in order of **due date** (earliest
  first), and items with the same due date in order of invoice number
  (ascending string comparison). `aging_report` passes its `as_of` through.

Tests: `tests/test_aging.py`.

---

## Issue 6: posting a foreign-currency entry fails

`Ledger.post_foreign(entry, rates, rounding_account)` raises
`UnbalancedEntryError: entry unbalanced by 8.00 EUR after conversion` for an
entry that debits bank USD 100.00 and credits sales EUR 92.00 at a rate of
0.92, which is balanced in EUR. Converted amounts are also sometimes a cent
off.

Acceptance:

- `JournalEntry.base_totals()` returns `(debit, credit)` summed over the
  lines' **base** amounts (`line.debit` / `line.credit`). `is_balanced` and
  `validate` use it, so the example above converts and posts.
- `fx.convert_amount(amount, rate, base)` computes `amount * rate` in
  `Decimal` and rounds half-up to the minor units of `base`:
  `convert_amount(Decimal("100.25"), Decimal("0.5"), "EUR")` is
  `Decimal("50.13")`.
- `fx.convert_entry(entry, rates, rounding_account=None)`: base-currency lines
  get `base_amount = amount`; foreign lines are converted with the rate in
  effect on the entry date. Let `d` be base debits minus base credits after
  conversion. If `d` is zero nothing is added. If `rounding_account` is given
  and `abs(d)` is **at most** 0.01 times the number of converted foreign lines,
  one line is appended: account `rounding_account`, amount and base amount
  `abs(d)`, no currency, memo `"FX rounding"`, on the credit side when `d > 0`
  and the debit side when `d < 0`. Otherwise `UnbalancedEntryError` is raised
  with the message `entry unbalanced by 0.03 EUR after conversion` (the
  absolute difference with the base currency's decimals, then the base
  currency code).

---

## Issue 7: CSV import stops at the first error and rejects "1,234.50"

`importer.parse_journal_csv(text, chart, base_currency="EUR")` in
`ledgerly/importer.py` rejects amounts with thousands separators and raises on
the first bad row, so users fix one row at a time. The line numbers in the
messages are also wrong.

Acceptance:

- Amount cells may contain commas as thousands separators: `"1,234.50"` is
  `Decimal("1234.50")` (surrounding whitespace is ignored as before).
- The whole file is read before anything is raised. If anything is wrong, a
  single `ImportErrors` is raised whose `errors` list contains every problem:
  first the row errors in file order, formatted `line N: <message>` where `N`
  is the line number in the file (the header is line 1; empty lines are
  skipped but still counted), with the existing messages (`invalid date
  '2024-13-01'`, `unknown account 9999`, `invalid amount 'abc'`,
  `both debit and credit given`, `neither debit nor credit given`,
  `missing entry reference`).
- After the row errors come the unbalanced entries, in the order the entries
  first appear, formatted `entry E2: unbalanced (debit 100.00, credit 90.00)`.
  An entry with at least one row error is not checked for balance (and gets
  no unbalanced message). Entries that use a foreign currency are not checked
  (as now).
- Without errors the function returns the entries exactly as before.

Tests: `tests/test_importer.py`.
