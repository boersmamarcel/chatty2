import unittest
from decimal import Decimal

from stockroom.catalog import Item
from stockroom.reorder import build_suggestions, suggest_qty


def item(sku="S", rp=50, lead=5, mult=10, moq=0, disc=False):
    return Item(sku, name=sku, reorder_point=rp, lead_time_days=lead, order_multiple=mult,
                min_order_qty=moq, discontinued=disc, unit_cost=Decimal("1.00"))


class Issue4Test(unittest.TestCase):
    def test_example(self):
        self.assertEqual(suggest_qty(item(mult=10), 30, 10, 3.2), 30)
        self.assertEqual(suggest_qty(item(mult=12, moq=100), 30, 10, 3.2), 108)

    def test_rounds_up(self):
        # target 50 + 15 = 65; position 40 -> gap 25
        self.assertEqual(suggest_qty(item(mult=10), 40, 0, 3.0), 30)
        # gap 21 -> 30
        self.assertEqual(suggest_qty(item(mult=10), 44, 0, 3.0), 30)
        # gap exactly a multiple
        self.assertEqual(suggest_qty(item(mult=5), 45, 0, 3.0), 20)

    def test_no_rounding_for_multiple_one_or_zero(self):
        self.assertEqual(suggest_qty(item(mult=1), 40, 0, 3.0), 25)
        self.assertEqual(suggest_qty(item(mult=0), 40, 0, 3.0), 25)

    def test_minimum_order_quantity(self):
        self.assertEqual(suggest_qty(item(mult=1, moq=40), 40, 0, 3.0), 40)
        self.assertEqual(suggest_qty(item(mult=10, moq=25), 50, 0, 1.0), 30)
        # gap above the minimum: the minimum does not apply
        self.assertEqual(suggest_qty(item(mult=10, moq=20), 40, 0, 3.0), 30)

    def test_trigger_unchanged(self):
        self.assertEqual(suggest_qty(item(), 51, 0, 3.0), 0)
        self.assertEqual(suggest_qty(item(), 30, 21, 3.0), 0)
        self.assertEqual(suggest_qty(item(mult=1), 50, 0, 3.0), 15)

    def test_discontinued(self):
        self.assertEqual(suggest_qty(item(disc=True), 0, 0, 3.0), 0)
        items = {"A": item("A", mult=1), "B": item("B", disc=True), "C": item("C", mult=1, moq=30)}
        got = build_suggestions(items, {"A": 40, "B": 0, "C": 45}, {}, {"A": 3.0, "B": 3.0, "C": 3.0})
        self.assertEqual([(s.sku, s.qty) for s in got], [("A", 25), ("C", 30)])
        self.assertEqual([(s.position, s.target) for s in got], [(40, 65), (45, 65)])


if __name__ == "__main__":
    unittest.main()
