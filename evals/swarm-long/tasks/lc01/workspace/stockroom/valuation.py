"""Stock valuation.

Two cost books are supported:

* :class:`AverageCostBook` -- perpetual weighted average cost, the method
  the finance team uses for the monthly stock value report;
* :class:`FifoCostBook` -- first-in-first-out cost layers, used for
  margin analysis of individual shipments.

Values are Decimals rounded to cents; the average unit cost is kept to four
decimals.
"""

import collections
from decimal import Decimal

from .money import ZERO, round_money, to_decimal

UNIT_COST_PLACES = 4


class ValuationError(ValueError):
    """Raised for impossible stock operations (e.g. issuing more than on hand)."""


class AverageCostBook(object):
    """Weighted average cost of one SKU.

    ``qty`` is the quantity on hand (eaches), ``value`` the stock value in
    cents, ``avg_cost`` the average unit cost (four decimals).
    """

    def __init__(self, sku):
        self.sku = sku
        self.qty = 0
        self.value = Decimal("0.00")
        self.avg_cost = Decimal("0")

    def __repr__(self):
        return "AverageCostBook(%r, qty=%r, value=%r, avg_cost=%r)" % (
            self.sku, self.qty, self.value, self.avg_cost)

    def receive(self, qty, unit_cost):
        """Add ``qty`` eaches bought at ``unit_cost`` and update the average."""
        qty = int(qty)
        if qty <= 0:
            raise ValuationError("receipt quantity must be positive")
        unit_cost = to_decimal(unit_cost)
        if unit_cost < 0:
            raise ValuationError("unit cost cannot be negative")
        self.value += round_money(qty * unit_cost)
        self.qty += qty
        self.avg_cost = Decimal(str(round(float(self.value) / self.qty, UNIT_COST_PLACES)))
        return self.avg_cost

    def issue(self, qty):
        """Remove ``qty`` eaches at the average cost; returns the cost of goods issued."""
        qty = int(qty)
        if qty <= 0:
            raise ValuationError("issue quantity must be positive")
        cogs = round_money(qty * self.avg_cost)
        self.qty -= qty
        self.value -= cogs
        return cogs

    def revalue(self, new_unit_cost):
        """Write the stock up or down to ``new_unit_cost``; returns the difference."""
        new_unit_cost = to_decimal(new_unit_cost)
        new_value = round_money(self.qty * new_unit_cost)
        diff = new_value - self.value
        self.value = new_value
        self.avg_cost = new_unit_cost.quantize(Decimal(1).scaleb(-UNIT_COST_PLACES))
        return diff


Layer = collections.namedtuple("Layer", "qty unit_cost")


class FifoCostBook(object):
    """FIFO cost layers of one SKU."""

    def __init__(self, sku):
        self.sku = sku
        self.layers = collections.deque()

    @property
    def qty(self):
        return sum(layer.qty for layer in self.layers)

    @property
    def value(self):
        return round_money(sum((layer.qty * layer.unit_cost for layer in self.layers), ZERO))

    def receive(self, qty, unit_cost):
        """Add a cost layer."""
        qty = int(qty)
        if qty <= 0:
            raise ValuationError("receipt quantity must be positive")
        self.layers.append(Layer(qty, to_decimal(unit_cost)))

    def issue(self, qty):
        """Consume the oldest layers first; returns the cost of goods issued."""
        qty = int(qty)
        if qty <= 0:
            raise ValuationError("issue quantity must be positive")
        if qty > self.qty:
            raise ValuationError("%s: cannot issue %d, only %d on hand" % (self.sku, qty, self.qty))
        cost = ZERO
        while qty:
            layer = self.layers[0]
            take = min(layer.qty, qty)
            cost += take * layer.unit_cost
            qty -= take
            if take == layer.qty:
                self.layers.popleft()
            else:
                self.layers[0] = Layer(layer.qty - take, layer.unit_cost)
        return round_money(cost)


def value_ledger(ledger, costs, method="average"):
    """Value every SKU of a :class:`~stockroom.ledger.StockLedger`.

    ``costs`` maps ``movement_id`` to the unit cost of each receipt. Picks
    and negative adjustments issue stock; positive adjustments are received
    at the current average cost (or the last layer's cost for FIFO).
    Returns ``{sku: book}``.
    """
    books = {}
    factory = AverageCostBook if method == "average" else FifoCostBook
    for m in ledger:
        book = books.setdefault(m.sku, factory(m.sku))
        if m.kind == "RECEIPT":
            book.receive(m.qty, costs.get(m.movement_id, ZERO))
        elif m.qty < 0:
            book.issue(-m.qty)
        elif m.qty > 0:
            if isinstance(book, AverageCostBook):
                book.receive(m.qty, book.avg_cost)
            else:
                last = book.layers[-1].unit_cost if book.layers else ZERO
                book.receive(m.qty, last)
    return books
