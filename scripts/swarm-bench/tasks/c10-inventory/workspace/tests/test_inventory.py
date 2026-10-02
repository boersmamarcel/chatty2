import unittest

from inventory import Inventory
from units import to_kg


class InventoryTest(unittest.TestCase):
    def test_grams(self):
        self.assertAlmostEqual(to_kg(2500, "g"), 2.5)

    def test_tonnes(self):
        self.assertAlmostEqual(to_kg(1.5, "t"), 1500.0)

    def test_receive_and_ship(self):
        inv = Inventory(reorder_at_kg=5)
        inv.receive("flour", 12, "kg")
        inv.ship("flour", 2000, "g")
        self.assertAlmostEqual(inv.stock_kg["flour"], 10.0)

    def test_cannot_overship(self):
        inv = Inventory(reorder_at_kg=5)
        inv.receive("salt", 1, "kg")
        with self.assertRaises(ValueError):
            inv.ship("salt", 1500, "g")

    def test_reorder_at_or_below(self):
        inv = Inventory(reorder_at_kg=5)
        inv.receive("flour", 5, "kg")
        inv.receive("sugar", 6, "kg")
        inv.receive("salt", 4000, "g")
        self.assertEqual(inv.to_reorder(), ["flour", "salt"])
