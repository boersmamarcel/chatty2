import unittest
from decimal import Decimal

from stockroom.money import allocate_amount, format_amount, round_money


class MoneyTest(unittest.TestCase):
    def test_round_half_up(self):
        self.assertEqual(round_money(Decimal("2.675")), Decimal("2.68"))

    def test_allocate_amount(self):
        self.assertEqual(allocate_amount(Decimal("10.00"), [1, 1, 1]),
                         [Decimal("3.34"), Decimal("3.33"), Decimal("3.33")])

    def test_format_small(self):
        self.assertEqual(format_amount(Decimal("5")), "5.00")

    def test_format_thousands(self):
        self.assertEqual(format_amount(Decimal("1234567.891")), "1,234,567.89")
        self.assertEqual(format_amount(Decimal("-1234.5")), "-1,234.50")


if __name__ == "__main__":
    unittest.main()
