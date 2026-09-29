"""Invoice totals for a small web shop."""

SHIPPING = 4.95
FREE_SHIPPING_FROM = 50.00


def line_total(unit_price, quantity, discount_pct=0):
    """One invoice line: unit price times quantity, less a percentage discount."""
    return round(unit_price * quantity * (1 - discount_pct / 100), 2)


def invoice_total(lines):
    """The lines' sum plus shipping. An order of FREE_SHIPPING_FROM or more ships free."""
    subtotal = round(sum(line_total(**line) for line in lines), 2)
    if subtotal > FREE_SHIPPING_FROM:
        return subtotal
    return round(subtotal + SHIPPING, 2)
