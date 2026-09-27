class Cart:
    CODES = ("SAVE10", "FLAT5")

    def __init__(self):
        self.lines = {}
        self.code = None

    def add(self, sku, price, qty=1):
        if price < 0 or qty < 1:
            raise ValueError("bad price or quantity")
        if sku in self.lines:
            self.lines[sku][1] += qty
        else:
            self.lines[sku] = [price, qty]

    def remove(self, sku):
        del self.lines[sku]

    def apply_code(self, code):
        if code not in self.CODES:
            raise ValueError("unknown code %r" % code)
        self.code = code

    def total(self):
        total = sum(price * qty for price, qty in self.lines.values())
        if self.code == "SAVE10":
            total *= 0.9
        elif self.code == "FLAT5":
            total = max(0.0, total - 5)
        return round(total, 2)
