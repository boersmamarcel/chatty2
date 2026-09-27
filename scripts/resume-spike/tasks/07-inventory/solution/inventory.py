class Inventory:
    def __init__(self):
        self.items = {}

    @staticmethod
    def _check(qty):
        if not isinstance(qty, int) or isinstance(qty, bool) or qty < 1:
            raise ValueError("quantity must be a positive int")

    def add(self, name, qty):
        self._check(qty)
        self.items[name] = self.items.get(name, 0) + qty

    def remove(self, name, qty):
        self._check(qty)
        have = self.items.get(name, 0)
        if qty > have:
            raise ValueError("only %d %s in stock" % (have, name))
        if qty == have:
            del self.items[name]
        else:
            self.items[name] = have - qty

    def count(self, name):
        return self.items.get(name, 0)

    def low_stock(self, threshold):
        return sorted(n for n, c in self.items.items() if c < threshold)

    def total(self):
        return sum(self.items.values())
