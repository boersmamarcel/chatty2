"""Sales invoices and the journal entries they produce.

An invoice line has a quantity, a unit price, a tax code and a revenue
account.  Posting an invoice debits accounts receivable with the gross total,
credits each revenue account with its net amount and credits the output VAT
account of each tax code with its tax.
"""

from .accounts import code_key
from .errors import ValidationError
from .journal import JournalEntry
from .money import ZERO, quantize, sum_amounts, to_decimal
from .tax import compute_tax


class InvoiceLine(object):
    """One line of an invoice."""

    def __init__(self, description, quantity, unit_price, tax_code="S", account="4000"):
        self.description = description
        self.quantity = to_decimal(quantity)
        self.unit_price = to_decimal(unit_price)
        self.tax_code = str(tax_code).strip().upper()
        self.account = str(account).strip()
        if self.quantity <= 0:
            raise ValidationError("quantity must be positive")
        if self.unit_price < 0:
            raise ValidationError("unit price cannot be negative")

    def net(self, currency=None):
        """``quantity * unit_price`` rounded half-up to the currency."""
        return quantize(self.quantity * self.unit_price, currency)


class Invoice(object):
    """A sales invoice to one customer."""

    def __init__(self, number, customer, date, due_date, lines=None, currency=None):
        self.number = number
        self.customer = customer
        self.date = date
        self.due_date = due_date
        self.lines = list(lines or [])
        self.currency = currency
        if due_date < date:
            raise ValidationError("invoice %s is due before it is issued" % number)

    def add_line(self, *args, **kwargs):
        self.lines.append(InvoiceLine(*args, **kwargs))
        return self

    def net_total(self):
        return sum_amounts(line.net(self.currency) for line in self.lines)

    def tax_breakdown(self, table):
        """``[(code, net, tax), ...]`` for every tax code used, sorted by code."""
        nets = {}
        for line in self.lines:
            code = table.get(line.tax_code)
            nets[code.code] = nets.get(code.code, ZERO) + line.net(self.currency)
        result = []
        for code in sorted(nets):
            rate = table.get(code).rate
            result.append((code, nets[code], compute_tax(nets[code], rate, self.currency)))
        return result

    def tax_total(self, table):
        return sum_amounts(tax for _code, _net, tax in self.tax_breakdown(table))

    def total(self, table):
        """Gross amount due: net total plus tax total."""
        return self.net_total() + self.tax_total(table)


def revenue_by_account(invoice):
    """``{account: net}`` over the invoice lines."""
    result = {}
    for line in invoice.lines:
        result[line.account] = result.get(line.account, ZERO) + line.net(invoice.currency)
    return result


def build_entry(invoice, table, receivable_account="1200"):
    """The journal entry posting ``invoice``.

    Lines, in order: one debit to ``receivable_account`` for the gross total;
    one credit per revenue account (code order) for its net amount; one
    credit per tax code with a non-zero tax (code order) to that code's
    account, with memo ``"VAT <code>"``.  Zero-rated and exempt codes produce
    no tax line.
    """
    if not invoice.lines:
        raise ValidationError("invoice %s has no lines" % invoice.number)
    entry = JournalEntry(invoice.date, "Invoice %s %s" % (invoice.number, invoice.customer),
                         reference=invoice.number)
    entry.debit(receivable_account, invoice.total(table), invoice.currency)
    revenue = revenue_by_account(invoice)
    for account in sorted(revenue, key=code_key):
        if revenue[account] > 0:
            entry.credit(account, revenue[account], invoice.currency)
    for code, _net, tax in invoice.tax_breakdown(table):
        tax_code = table.get(code)
        if tax > 0 and not tax_code.exempt:
            entry.credit(tax_code.account, tax, invoice.currency, memo="VAT %s" % code)
    return entry


def credit_note_entry(invoice, table, receivable_account="1200"):
    """The reversing entry of :func:`build_entry` (every side swapped)."""
    original = build_entry(invoice, table, receivable_account)
    entry = JournalEntry(invoice.date, "Credit note %s" % invoice.number, reference=invoice.number)
    for line in original.lines:
        if line.side == "D":
            entry.credit(line.account, line.amount, line.currency, memo=line.memo)
        else:
            entry.debit(line.account, line.amount, line.currency, memo=line.memo)
    return entry
