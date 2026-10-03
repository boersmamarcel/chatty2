"""Replenishment suggestions.

The planner runs this every morning: for each SKU it compares the stock
position (on hand plus open purchase orders) with the item's reorder point
and proposes an order quantity.
"""

import collections
import math

Suggestion = collections.namedtuple("Suggestion", "sku position target qty")


def target_level(item, daily_demand):
    """Stock level an order should bring the position up to.

    The reorder point plus the expected demand over the supplier lead time,
    rounded up to whole eaches.
    """
    return item.reorder_point + int(math.ceil(daily_demand * item.lead_time_days))


def suggest_qty(item, on_hand, on_order, daily_demand):
    """Order quantity for ``item`` (0 if no order is needed).

    An order is needed when the stock position (``on_hand + on_order``) is at
    or below the reorder point. The quantity covers the gap up to
    :func:`target_level` and is adjusted to the supplier's order multiple.
    """
    position = on_hand + on_order
    if position > item.reorder_point:
        return 0
    need = target_level(item, daily_demand) - position
    if need <= 0:
        return 0
    multiple = item.order_multiple or 1
    return int(round(float(need) / multiple)) * multiple


def build_suggestions(items, on_hand, on_order, demand):
    """Suggestions for every item that needs an order, sorted by SKU.

    ``on_hand``, ``on_order`` and ``demand`` are dicts keyed by SKU; missing
    SKUs count as zero.
    """
    out = []
    for sku in sorted(items):
        item = items[sku]
        oh = on_hand.get(sku, 0)
        oo = on_order.get(sku, 0)
        dd = demand.get(sku, 0.0)
        qty = suggest_qty(item, oh, oo, dd)
        if qty > 0:
            out.append(Suggestion(sku, oh + oo, target_level(item, dd), qty))
    return out


def order_value(suggestions, items):
    """Total purchase value of ``suggestions`` at catalog unit cost."""
    from .money import round_money, ZERO
    total = ZERO
    for s in suggestions:
        total += s.qty * items[s.sku].unit_cost
    return round_money(total)
