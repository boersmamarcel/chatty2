"""Accounts-receivable aging and customer statements.

Open items (unpaid invoices) are aged by the number of days they are overdue
on the report date and grouped into buckets.  Customer payments are not
matched to specific invoices; they are applied to the customer's open items
automatically (see :func:`allocate_payments`).
"""

from .formatting import format_amount, format_date, format_table
from .money import ZERO, sum_amounts, to_decimal

BUCKETS = ("current", "1-30", "31-60", "61-90", "90+")


class OpenItem(object):
    """An invoice that may still be (partly) unpaid."""

    __slots__ = ("number", "customer", "invoice_date", "due_date", "amount")

    def __init__(self, number, customer, invoice_date, due_date, amount):
        self.number = number
        self.customer = customer
        self.invoice_date = invoice_date
        self.due_date = due_date
        self.amount = to_decimal(amount)

    @classmethod
    def from_invoice(cls, invoice, table):
        """Open item for a :class:`ledgerly.invoicing.Invoice`."""
        return cls(invoice.number, invoice.customer, invoice.date, invoice.due_date,
                   invoice.total(table))

    def __repr__(self):
        return "OpenItem(%r, %r, %s)" % (self.number, self.customer, self.amount)


class Payment(object):
    """A payment received from a customer."""

    __slots__ = ("customer", "date", "amount", "reference")

    def __init__(self, customer, date, amount, reference=""):
        self.customer = customer
        self.date = date
        self.amount = to_decimal(amount)
        self.reference = reference


def days_overdue(item, as_of):
    """Days between the item's due date and ``as_of`` (negative if not yet due)."""
    return (as_of - item.invoice_date).days


def bucket_for(days):
    """The aging bucket name for a number of days overdue."""
    if days <= 0:
        return "current"
    if days < 30:
        return "1-30"
    if days < 60:
        return "31-60"
    if days < 90:
        return "61-90"
    return "90+"


def allocate_payments(items, payments, as_of=None):
    """Apply payments to open items; returns ``{item number: remaining}``.

    Each customer's payments are pooled and applied to that customer's
    items oldest first.  Remaining amounts are never negative; an
    over-payment is simply left unapplied.
    """
    remaining = dict((item.number, item.amount) for item in items)
    pool = {}
    for payment in payments:
        pool[payment.customer] = pool.get(payment.customer, ZERO) + payment.amount
    for item in sorted(items, key=lambda i: i.invoice_date):
        credit = pool.get(item.customer, ZERO)
        if credit <= 0:
            continue
        applied = min(credit, remaining[item.number])
        remaining[item.number] -= applied
        pool[item.customer] = credit - applied
    return remaining


class AgingReport(object):
    """Bucket totals per customer.

    ``rows`` is a list of ``(customer, {bucket: amount})`` ordered by customer
    name; every bucket key is present.  ``items`` lists
    ``(item, remaining, bucket)`` for items with something left to pay.
    """

    def __init__(self, as_of, rows, items):
        self.as_of = as_of
        self.rows = rows
        self.items = items

    def customer(self, name):
        for customer, buckets in self.rows:
            if customer == name:
                return buckets
        raise KeyError(name)

    def totals(self):
        """``{bucket: amount}`` over all customers."""
        return dict((b, sum_amounts(buckets[b] for _c, buckets in self.rows)) for b in BUCKETS)

    def total(self):
        return sum_amounts(self.totals().values())


def aging_report(items, payments, as_of):
    """Build an :class:`AgingReport` on ``as_of``."""
    remaining = allocate_payments(items, payments, as_of)
    per_customer = {}
    open_items = []
    for item in items:
        left = remaining[item.number]
        if left <= 0:
            continue
        bucket = bucket_for(days_overdue(item, as_of))
        buckets = per_customer.setdefault(item.customer, dict((b, ZERO) for b in BUCKETS))
        buckets[bucket] += left
        open_items.append((item, left, bucket))
    rows = [(customer, per_customer[customer]) for customer in sorted(per_customer)]
    return AgingReport(as_of, rows, open_items)


def render_aging(report, currency=None):
    """Plain-text aging table, one row per customer plus a ``TOTAL`` row."""
    body = []
    for customer, buckets in report.rows:
        body.append([customer] + [format_amount(buckets[b], currency) for b in BUCKETS])
    totals = report.totals()
    body.append(["TOTAL"] + [format_amount(totals[b], currency) for b in BUCKETS])
    title = "Aging as of %s" % format_date(report.as_of)
    table = format_table(["Customer"] + list(BUCKETS), body, ["<"] + [">"] * len(BUCKETS))
    return title + "\n" + table


def statement_lines(customer, items, payments, as_of):
    """Chronological statement of one customer up to ``as_of`` (inclusive).

    Returns ``(date, description, charge, payment, balance)`` tuples:
    invoices are charges, payments reduce the balance.  On the same day,
    invoices come before payments.
    """
    events = []
    for item in items:
        if item.customer == customer and item.invoice_date <= as_of:
            events.append((item.invoice_date, 0, "Invoice %s" % item.number, item.amount, ZERO))
    for payment in payments:
        if payment.customer == customer and payment.date <= as_of:
            label = "Payment %s" % payment.reference if payment.reference else "Payment"
            events.append((payment.date, 1, label, ZERO, payment.amount))
    events.sort(key=lambda e: (e[0], e[1]))
    balance = ZERO
    lines = []
    for day, _kind, text, charge, paid in events:
        balance += charge - paid
        lines.append((day, text, charge, paid, balance))
    return lines
