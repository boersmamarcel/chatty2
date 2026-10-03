"""CSV import of an expense list.

Columns: date,payer,amount,currency,description,split

* `amount` is a decimal amount with at most two decimals;
* `currency` may be empty (the group currency); other currencies are
  converted to the group currency before the split, using the rates;
* `split` is a split specification, see `splitbill.expenses`.

Blank lines are ignored. Errors name the line in the file (header = 1).
"""

import csv
import io

from .currency import CurrencyError, convert
from .expenses import Expense, SplitError, parse_split
from .money import MoneyError, parse_amount

COLUMNS = ("date", "payer", "amount", "currency", "description", "split")


class ImportFailed(ValueError):
    """Raised for a malformed expense file."""


def load_expenses(text, group_currency="EUR", rates=None):
    """List of Expense in file order, amounts in the group currency."""
    reader = csv.reader(io.StringIO(text))
    header = next(reader, None)
    if header is None or [h.strip().lower() for h in header] != list(COLUMNS):
        raise ImportFailed("line 1: expected header %s" % ",".join(COLUMNS))
    group_currency = group_currency.upper()
    expenses = []
    for row in reader:
        line_no = reader.line_num
        if not any(cell.strip() for cell in row):
            continue
        if len(row) != len(COLUMNS):
            raise ImportFailed("line %d: expected %d fields" % (line_no, len(COLUMNS)))
        date, payer, amount_text, currency, description, split = [c.strip() for c in row]
        try:
            amount = parse_amount(amount_text)
            if amount == 0:
                raise ImportFailed("line %d: amount must not be zero" % line_no)
            currency = (currency or group_currency).upper()
            if currency != group_currency:
                amount = convert(amount, currency, group_currency, rates or {})
            shares = parse_split(split, amount)
        except (MoneyError, CurrencyError, SplitError) as exc:
            raise ImportFailed("line %d: %s" % (line_no, exc))
        payer = payer.lower()
        if not payer:
            raise ImportFailed("line %d: empty payer" % line_no)
        expenses.append(Expense(date, payer, amount, description, shares, group_currency))
    return expenses
