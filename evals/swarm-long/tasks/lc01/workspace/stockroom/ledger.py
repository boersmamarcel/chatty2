"""The stock movement ledger.

Every change to stock is a :class:`Movement`. Quantities are signed eaches
from the warehouse's point of view: receipts are positive, picks negative,
adjustments either sign. The ledger is append-only; corrections are new
adjustment movements.
"""

import collections
import datetime

from .lots import Lot

KINDS = ("RECEIPT", "PICK", "ADJUST", "TRANSFER")

Movement = collections.namedtuple(
    "Movement", "movement_id date sku kind qty lot expiry bin note")
Movement.__new__.__defaults__ = (None, None, None, "")


class LedgerError(ValueError):
    """Raised for movements the ledger refuses."""


def make_movement(movement_id, date, sku, kind, qty, lot=None, expiry=None, bin=None, note=""):
    """Validate and build a :class:`Movement` (the sign of ``qty`` follows ``kind``).

    ``qty`` is given as a positive number for receipts and picks; picks are
    stored negative. Adjustments keep their sign.
    """
    kind = kind.strip().upper()
    if kind not in KINDS:
        raise LedgerError("unknown movement kind %r" % kind)
    if not isinstance(date, datetime.date):
        raise LedgerError("movement %s: date must be a date" % movement_id)
    qty = int(qty)
    if kind in ("RECEIPT", "PICK") and qty <= 0:
        raise LedgerError("movement %s: %s quantity must be positive" % (movement_id, kind.lower()))
    if kind == "ADJUST" and qty == 0:
        raise LedgerError("movement %s: zero adjustment" % movement_id)
    if kind == "PICK":
        qty = -qty
    return Movement(movement_id, date, sku.strip().upper(), kind, qty, lot, expiry, bin, note)


class StockLedger(object):
    """An in-memory ledger with on-hand queries."""

    def __init__(self, allow_negative=False):
        self.allow_negative = allow_negative
        self._movements = []
        self._ids = set()

    def __len__(self):
        return len(self._movements)

    def __iter__(self):
        return iter(self._movements)

    def post(self, movement):
        """Append a movement, refusing duplicates and (by default) negative stock."""
        if movement.movement_id in self._ids:
            raise LedgerError("duplicate movement id %s" % movement.movement_id)
        if movement.qty < 0 and not self.allow_negative:
            available = self.on_hand(movement.sku, lot=movement.lot)
            if available + movement.qty < 0:
                raise LedgerError("movement %s would take %s below zero (%d on hand)" % (
                    movement.movement_id, movement.sku, available))
        self._movements.append(movement)
        self._ids.add(movement.movement_id)
        return movement

    def post_all(self, movements):
        """Post several movements in date order (stable for equal dates)."""
        for m in sorted(movements, key=lambda m: m.date):
            self.post(m)

    def movements_for(self, sku, start=None, end=None):
        """Movements of ``sku`` with ``start <= date <= end`` (either bound optional)."""
        sku = sku.upper()
        out = []
        for m in self._movements:
            if m.sku != sku:
                continue
            if start is not None and m.date < start:
                continue
            if end is not None and m.date > end:
                continue
            out.append(m)
        return out

    def on_hand(self, sku, lot=None, as_of=None):
        """Quantity on hand of ``sku`` (optionally one lot) at the end of ``as_of``."""
        total = 0
        for m in self.movements_for(sku, end=as_of):
            if lot is not None and m.lot != lot:
                continue
            total += m.qty
        return total

    def skus(self):
        """All SKUs that have movements, sorted."""
        return sorted({m.sku for m in self._movements})

    def lots_for(self, sku, as_of=None):
        """Current :class:`Lot` records of ``sku`` with stock, sorted by lot id.

        A lot's expiry and bin come from its first receipt, its received date
        is the date of its first receipt.
        """
        info = collections.OrderedDict()
        qty = collections.defaultdict(int)
        for m in self.movements_for(sku, end=as_of):
            key = m.lot or ""
            qty[key] += m.qty
            if key not in info and m.kind == "RECEIPT":
                info[key] = (m.expiry, m.date, m.bin)
        lots = []
        for key in sorted(qty):
            if qty[key] <= 0:
                continue
            expiry, received, bin_code = info.get(key, (None, None, None))
            lots.append(Lot(key, sku.upper(), qty[key], expiry=expiry, received=received,
                            bin=bin_code))
        return lots

    def daily_demand(self, sku, end, days=28):
        """Average picked eaches per day over the ``days`` days ending on ``end``."""
        start = end - datetime.timedelta(days=days - 1)
        picked = -sum(m.qty for m in self.movements_for(sku, start=start, end=end)
                      if m.kind == "PICK")
        return picked / float(days)
