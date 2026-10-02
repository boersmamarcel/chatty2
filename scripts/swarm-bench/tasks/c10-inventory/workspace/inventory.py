"""Stock levels and reorders."""

from units import to_kg


class Inventory(object):
    def __init__(self, reorder_at_kg):
        self.reorder_at_kg = reorder_at_kg
        self.stock_kg = {}

    def receive(self, item, amount, unit):
        self.stock_kg[item] = self.stock_kg.get(item, 0.0) + to_kg(amount, unit)

    def ship(self, item, amount, unit):
        kg = to_kg(amount, unit)
        if kg > self.stock_kg.get(item, 0.0):
            raise ValueError("not enough %s" % item)
        self.stock_kg[item] -= kg

    def to_reorder(self):
        """Items at or below the reorder level, sorted."""
        return sorted(i for i, kg in self.stock_kg.items() if kg < self.reorder_at_kg)
