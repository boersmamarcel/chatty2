class Cart:
    def add(self, sku, price, qty=1):
        raise NotImplementedError

    def remove(self, sku):
        raise NotImplementedError

    def total(self):
        raise NotImplementedError
