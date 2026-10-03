"""Lots (batches) of a SKU.

Every receipt creates or tops up a lot. A lot has an optional expiry date
(non-perishables have none) and the date it was first received.
"""

import datetime


class LotError(ValueError):
    """Raised for invalid lot data."""


class Lot(object):
    """Stock of one SKU from one batch, with its quantity on hand (eaches)."""

    __slots__ = ("lot_id", "sku", "qty", "expiry", "received", "bin")

    def __init__(self, lot_id, sku, qty, expiry=None, received=None, bin=None):
        if qty < 0:
            raise LotError("lot %s: negative quantity %r" % (lot_id, qty))
        self.lot_id = lot_id
        self.sku = sku
        self.qty = qty
        self.expiry = expiry
        self.received = received
        self.bin = bin

    def __repr__(self):
        return "Lot(%r, %r, qty=%r, expiry=%r, received=%r)" % (
            self.lot_id, self.sku, self.qty, self.expiry, self.received)

    def is_expired(self, on_date):
        """True if the lot can no longer be used on ``on_date``.

        A lot is usable up to and including the day before its expiry date.
        """
        return self.expiry is not None and self.expiry <= on_date

    def days_left(self, on_date):
        """Days until expiry counted from ``on_date``; ``None`` if it never expires."""
        if self.expiry is None:
            return None
        return (self.expiry - on_date).days


def parse_date(text):
    """Parse an ISO date (``YYYY-MM-DD``); empty text gives ``None``."""
    text = (text or "").strip()
    if not text:
        return None
    try:
        return datetime.datetime.strptime(text, "%Y-%m-%d").date()
    except ValueError:
        raise LotError("bad date: %r" % (text,))


def lot_from_row(row):
    """Build a :class:`Lot` from a dict with ``lot_id, sku, qty, expiry, received``."""
    try:
        qty = int(row.get("qty") or 0)
    except ValueError:
        raise LotError("lot %s: bad quantity %r" % (row.get("lot_id"), row.get("qty")))
    return Lot(
        lot_id=(row.get("lot_id") or "").strip(),
        sku=(row.get("sku") or "").strip().upper(),
        qty=qty,
        expiry=parse_date(row.get("expiry")),
        received=parse_date(row.get("received")),
        bin=(row.get("bin") or "").strip() or None,
    )


def expiring_within(lots, on_date, days):
    """Lots with stock that expire within ``days`` days after ``on_date``.

    Sorted by expiry date, then lot id.
    """
    limit = on_date + datetime.timedelta(days=days)
    hits = [lot for lot in lots
            if lot.qty > 0 and lot.expiry is not None and on_date < lot.expiry <= limit]
    return sorted(hits, key=lambda lot: (lot.expiry, lot.lot_id))


def total_qty(lots):
    """Sum of the quantities of ``lots``."""
    return sum(lot.qty for lot in lots)
