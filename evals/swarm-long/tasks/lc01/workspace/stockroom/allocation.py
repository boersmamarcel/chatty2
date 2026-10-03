"""FEFO allocation: decide which lots a pick is taken from.

First-expiry-first-out: the lot that expires first is picked first, so that
nothing expires on the shelf. Lots without an expiry date are used last.
"""

import collections

Allocation = collections.namedtuple("Allocation", "lot_id qty bin")


class AllocationError(ValueError):
    """Raised when the eligible lots cannot cover the requested quantity.

    ``shortfall`` is the number of eaches that could not be allocated.
    """

    def __init__(self, sku, requested, shortfall):
        ValueError.__init__(self, "%s: cannot allocate %d (short by %d)" % (
            sku, requested, shortfall))
        self.sku = sku
        self.requested = requested
        self.shortfall = shortfall


def eligible_lots(lots, ship_date, min_remaining_days=0):
    """The lots a pick shipping on ``ship_date`` may use.

    A lot is skipped when it has no stock, when it has expired by the ship
    date, or when fewer than ``min_remaining_days`` days of shelf life are
    left on the ship date (customers often require a minimum remaining shelf
    life).
    """
    out = []
    for lot in lots:
        if lot.qty <= 0:
            continue
        if lot.expiry is not None:
            if lot.expiry < ship_date:
                continue
            if (lot.expiry - ship_date).days < min_remaining_days:
                continue
        out.append(lot)
    return out


def fefo_order(lots):
    """Sort lots first-expiry-first-out."""
    def key(lot):
        has_expiry = 0 if lot.expiry is not None else 1
        return (has_expiry, lot.expiry or lot.received, lot.lot_id)
    return sorted(lots, key=key)


def allocate(sku, lots, qty, ship_date, min_remaining_days=0):
    """Allocate ``qty`` eaches of ``sku`` over ``lots``.

    Returns a list of :class:`Allocation` in picking order. Raises
    :class:`AllocationError` if the eligible stock is insufficient; nothing is
    allocated in that case.
    """
    if qty <= 0:
        raise ValueError("quantity to allocate must be positive")
    remaining = qty
    result = []
    for lot in fefo_order(eligible_lots(lots, ship_date, min_remaining_days)):
        if remaining == 0:
            break
        take = min(lot.qty, remaining)
        result.append(Allocation(lot.lot_id, take, lot.bin))
        remaining -= take
    if remaining:
        raise AllocationError(sku, qty, remaining)
    return result


def allocate_order(order_lines, lots_by_sku, ship_date, min_remaining_days=0):
    """Allocate every ``(sku, qty)`` line of an order.

    Lines for the same SKU draw from the same stock, so quantities taken by
    an earlier line are not available to a later one. Returns
    ``{sku: [Allocation, ...]}`` merged per SKU.
    """
    working = {}
    for sku, lots in lots_by_sku.items():
        working[sku] = [_copy_lot(lot) for lot in lots]
    result = collections.OrderedDict()
    for sku, qty in order_lines:
        lots = working.get(sku, [])
        picks = allocate(sku, lots, qty, ship_date, min_remaining_days)
        by_id = {lot.lot_id: lot for lot in lots}
        for a in picks:
            by_id[a.lot_id].qty -= a.qty
        result.setdefault(sku, []).extend(picks)
    return result


def _copy_lot(lot):
    from .lots import Lot
    return Lot(lot.lot_id, lot.sku, lot.qty, expiry=lot.expiry, received=lot.received, bin=lot.bin)
