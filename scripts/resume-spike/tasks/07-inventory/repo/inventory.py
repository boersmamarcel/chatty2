class Inventory:
    def __init__(self):
        self.items = {}

    def add(self, name, qty):
        self.items[name] = self.items.get(name, 0) + qty

    def remove(self, name, qty):
        self.items[name] = self.items.get(name, 0) - qty

    def count(self, name):
        return self.items.get(name, 0)
