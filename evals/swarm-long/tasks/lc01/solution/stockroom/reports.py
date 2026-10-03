"""Plain-text reports for the stock controller's morning e-mail."""

from .money import ZERO, format_amount, round_money
from .units import describe


def _table(header, rows):
    """Render rows as left-aligned columns separated by two spaces."""
    widths = [len(h) for h in header]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(cell))
    lines = ["  ".join(h.ljust(widths[i]) for i, h in enumerate(header)).rstrip()]
    for row in rows:
        lines.append("  ".join(c.ljust(widths[i]) for i, c in enumerate(row)).rstrip())
    return "\n".join(lines)


def stock_value_report(rows):
    """The stock value report.

    ``rows`` is a list of ``(sku, qty, unit_cost)``; the value of a row is
    ``qty * unit_cost`` rounded to cents. Rows are printed in the given order
    followed by a TOTAL row.
    """
    cells = [("SKU", "QTY", "VALUE")]
    total_qty = 0
    total_value = ZERO
    for sku, qty, unit_cost in rows:
        value = round_money(qty * unit_cost)
        total_qty += qty
        total_value += value
        cells.append((sku, str(qty), format_amount(value, accounting=True)))
    cells.append(("TOTAL", str(total_qty), format_amount(total_value, accounting=True)))
    widths = [max(len(c[i]) for c in cells) for i in range(3)]
    lines = ["%s  %s  %s" % (c[0].ljust(widths[0]), c[1].rjust(widths[1]), c[2].rjust(widths[2]))
             for c in cells]
    return "\n".join(line.rstrip() for line in lines)


def on_hand_report(ledger, catalog, as_of=None):
    """On-hand quantities per SKU with a box/case breakdown."""
    rows = []
    for sku in ledger.skus():
        qty = ledger.on_hand(sku, as_of=as_of)
        item = catalog.get(sku)
        breakdown = describe(qty, item) if item is not None else "%d EA" % qty
        rows.append([sku, str(qty), breakdown])
    return _table(["SKU", "ON HAND", "BREAKDOWN"], rows)


def expiry_report(lots, on_date, days=30):
    """Lots expiring within ``days`` days, soonest first."""
    from .lots import expiring_within
    rows = []
    for lot in expiring_within(lots, on_date, days):
        rows.append([lot.sku, lot.lot_id, lot.expiry.isoformat(), str(lot.days_left(on_date)),
                     str(lot.qty)])
    if not rows:
        return "No lots expire within %d days of %s." % (days, on_date.isoformat())
    return _table(["SKU", "LOT", "EXPIRY", "DAYS", "QTY"], rows)


def reorder_report(suggestions, items):
    """The purchasing list: one line per suggestion and the order value."""
    from .reorder import order_value
    rows = [[s.sku, items[s.sku].name, str(s.position), str(s.target), str(s.qty)]
            for s in suggestions]
    body = _table(["SKU", "NAME", "POSITION", "TARGET", "ORDER"], rows)
    return body + "\nOrder value: " + format_amount(order_value(suggestions, items))
