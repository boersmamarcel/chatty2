"""Import journal entries from CSV.

The file has a header row with the columns ``entry, date, account, debit,
credit, currency, description`` (any order; extra columns are ignored).
Rows sharing the same ``entry`` value form one journal entry; entries are
returned in the order their first row appears.  The entry date is the date of
its first row; the description is the first non-empty description.

Every value is stripped of surrounding whitespace.  Dates are ISO
(``2024-03-05``) or day-first with dots (``05.03.2024``).  Amounts use a dot
as decimal separator; an empty amount means zero.  ``currency`` may be empty
(the base currency).
"""

import csv
import datetime
import io
from decimal import Decimal, InvalidOperation

from .errors import ImportErrors
from .journal import JournalEntry
from .money import ZERO, quantize

REQUIRED_COLUMNS = ("entry", "date", "account", "debit", "credit", "currency", "description")


def parse_amount(text):
    """Parse an amount cell; empty means zero.  Raises ``ValueError``."""
    cleaned = text.strip()
    if not cleaned:
        return ZERO
    try:
        value = Decimal(cleaned)
    except InvalidOperation:
        raise ValueError("invalid amount %r" % (text.strip(),))
    if value < 0:
        raise ValueError("negative amount %r" % (text.strip(),))
    return value


def parse_date(text):
    """Parse ``YYYY-MM-DD`` or ``DD.MM.YYYY``.  Raises ``ValueError``."""
    cleaned = text.strip()
    for fmt in ("%Y-%m-%d", "%d.%m.%Y"):
        try:
            return datetime.datetime.strptime(cleaned, fmt).date()
        except ValueError:
            continue
    raise ValueError("invalid date %r" % (cleaned,))


def _parse_row(row, chart):
    """Validate one row; returns ``(date, account, side, amount)``."""
    day = parse_date(row["date"])
    account = row["account"].strip()
    if account not in chart:
        raise ValueError("unknown account %s" % account)
    debit = parse_amount(row["debit"])
    credit = parse_amount(row["credit"])
    if debit and credit:
        raise ValueError("both debit and credit given")
    if not debit and not credit:
        raise ValueError("neither debit nor credit given")
    if debit:
        return day, account, "D", debit
    return day, account, "C", credit


def parse_journal_csv(text, chart, base_currency="EUR"):
    """Parse CSV ``text`` into a list of unposted :class:`JournalEntry`.

    Raises :class:`ImportErrors` when the file is invalid.
    """
    reader = csv.DictReader(io.StringIO(text))
    fields = [f.strip() for f in (reader.fieldnames or [])]
    missing = [c for c in REQUIRED_COLUMNS if c not in fields]
    if missing:
        raise ImportErrors(["line 1: missing column %s" % c for c in missing])
    reader.fieldnames = fields
    entries = {}
    order = []
    for line_no, row in enumerate(reader, start=1):
        ref = (row["entry"] or "").strip()
        try:
            if not ref:
                raise ValueError("missing entry reference")
            day, account, side, amount = _parse_row(row, chart)
        except ValueError as exc:
            raise ImportErrors(["line %d: %s" % (line_no, exc)])
        currency = (row["currency"] or "").strip() or None
        if currency is not None and currency.upper() == base_currency:
            currency = None
        description = (row["description"] or "").strip()
        entry = entries.get(ref)
        if entry is None:
            entry = entries[ref] = JournalEntry(day, description, reference=ref)
            order.append(ref)
        elif not entry.description and description:
            entry.description = description
        if side == "D":
            entry.debit(account, amount, currency)
        else:
            entry.credit(account, amount, currency)
    for ref in order:
        entry = entries[ref]
        if entry.currencies():
            continue
        debit, credit = entry.totals()
        if debit != credit:
            raise ImportErrors(["entry %s: unbalanced (debit %s, credit %s)" % (
                ref, quantize(debit), quantize(credit))])
    return [entries[ref] for ref in order]
