# Open issues

Five open issues, reported by users of the shared-expenses tool. They are
independent of each other. Each lists its acceptance criteria; the behaviour
described there is what will be checked, including every edge case listed.

---

## Issue 1: equal splits give all leftover cents to the last person

Reported by: a flatmate

Splitting 10.00 three ways gives `3.33, 3.33, 3.34`; the house rule (and
the user guide) says the first people listed pay the extra cents. A refund
of -1.00 split three ways comes out as `-0.34, -0.34, -0.32`, which is
plainly wrong.

Acceptance (`splitbill.money.split_even(total, n)`):

- Returns `n` int shares adding up to `total`, each the exact share rounded
  towards zero, with one extra cent for each of the first
  `abs(total) % n` shares: `split_even(1000, 3)` -> `[334, 333, 333]`,
  `split_even(101, 4)` -> `[26, 25, 25, 25]`, `split_even(2, 5)` ->
  `[1, 1, 0, 0, 0]`.
- A negative total mirrors the positive one: `split_even(-100, 3)` ->
  `[-34, -33, -33]`, `split_even(-1, 2)` -> `[-1, 0]`.
- `n <= 0` still raises `MoneyError`.

## Issue 2: percentage splits lose or invent cents

Reported by: trip organiser

`percent:anna=33.33,ben=33.33,cleo=33.34` on 10.00 gives shares that add up
to 9.99 or 10.01, and `percent:anna=60,ben=30` (90 %) is accepted silently.

Acceptance (`splitbill.expenses.split_percent(total, percents)`, `percents`
maps name -> percentage as `Decimal`, int or numeric string):

- The percentages must add up to exactly 100 and none may be negative;
  otherwise `SplitError` is raised.
- The shares are computed with the largest-remainder method, like
  `splitbill.money.split_weighted`: every share gets its exact amount
  rounded towards zero, the cents left over go one each to the largest
  fractional remainders, ties to the person listed first; negative totals
  are split like the positive total and negated. The shares always add up
  to `total`.
- Examples: 1000 at 33.33/33.33/33.34 -> 333/333/334; 1001 at 50/50 ->
  501/500; 1 at 50/50 -> 1/0; -1001 at 50/50 -> -501/-500.
- The result is an OrderedDict in the given order with stripped,
  lower-cased names (as before).

## Issue 3: settling up produces too many transfers

Reported by: trip organiser

After a weekend trip with balances anna +50.00, ben +10.00, cleo -30.00,
dirk -30.00 the app told cleo to pay twice. The documented strategy (see the
`splitbill.settle` docstring) is not what the code does. Balances that do
not add up to zero (a corrupted export) are "settled" without complaint.

Acceptance (`splitbill.settle.settle(balances)`):

- Repeatedly, the debtor who owes the most pays the creditor who is owed
  the most, the smaller of the two amounts; ties between equal amounts go to
  the name that sorts first. People with a zero balance never appear. The
  result is the list of `(debtor, creditor, cents)` in that order.
- Example above: `[("cleo", "anna", 3000), ("dirk", "anna", 2000),
  ("dirk", "ben", 1000)]`.
- If the balances do not add up to zero, `SettleError` is raised. All-zero
  or empty balances give `[]`.

## Issue 4: foreign-currency expenses are converted the wrong way round

Reported by: a user back from the US

Rates are quoted as "1 EUR = x units" (see the `splitbill.currency`
docstring), but `convert` multiplies where it should divide, so 100.00 USD
became 108.50 EUR instead of 92.17 EUR. The result is also truncated rather
than rounded, lower-case codes fail, and an unknown currency raises a bare
`KeyError`.

Acceptance (`splitbill.currency.convert(cents, src, dst, rates)`):

- `cents` of `src` is converted to EUR by dividing by the `src` rate, then
  to `dst` by multiplying by the `dst` rate (EUR has rate 1 even when it is
  not in `rates`), and rounded half-up (away from zero) to whole cents:
  with `USD 1.0850`, `convert(10000, "USD", "EUR")` -> 9217; with
  `SEK 10`, `convert(25, "SEK", "EUR")` -> 3 and `convert(-25, "SEK",
  "EUR")` -> -3; with `USD 1.25, GBP 0.86`, `convert(1000, "USD", "GBP")`
  -> 688.
- Codes are case-insensitive (`"usd"`); a code that is not in `rates` (and
  not EUR) raises `CurrencyError`, also when `src == dst`. Converting a
  known currency to itself returns `cents` unchanged.

## Issue 5: refunds cannot be recorded

Reported by: a flatmate

When the landlord refunded 30.00 of the deposit to anna, we tried to enter
it as a negative expense shared by everybody, as the user guide suggests:

    2026-03-04,anna,-30.00,,deposit refund,"equal:anna,ben,cleo"

The import rejects the line ("amount must be positive"), and building
balances from such an expense raises `BalanceError` as well.

Acceptance:

- `splitbill.importer.load_expenses` accepts a negative amount (a refund)
  with any split method; the shares are then negative and add up to the
  amount. An amount of exactly zero is still an error
  (`ImportFailed`, message `"line <n>: ..."`).
- `splitbill.balances.net_balances` accepts expenses with a negative amount
  and books them with the same rule as any expense (payer += amount, each
  person -= share): after the refund above anna's balance goes down by
  20.00 (she holds 30.00 of which 10.00 is hers) and ben's and cleo's go
  up by 10.00 each. An expense with amount 0
  still raises `BalanceError`, and so do shares that do not add up to the
  amount.
