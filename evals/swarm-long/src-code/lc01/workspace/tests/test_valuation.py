import unittest
from decimal import Decimal

from stockroom.valuation import AverageCostBook


class AverageCostTest(unittest.TestCase):
    def test_average_after_two_receipts(self):
        book = AverageCostBook("SKU1")
        book.receive(10, Decimal("2.00"))
        book.receive(30, Decimal("3.00"))
        self.assertEqual(book.value, Decimal("110.00"))
        self.assertEqual(book.avg_cost, Decimal("2.7500"))

    def test_average_is_rounded_half_up(self):
        book = AverageCostBook("SKU1")
        book.receive(32, Decimal("0.03125"))
        self.assertEqual(book.value, Decimal("1.00"))
        self.assertEqual(book.avg_cost, Decimal("0.0313"))

    def test_issue_costs_average(self):
        book = AverageCostBook("SKU1")
        book.receive(10, Decimal("2.00"))
        self.assertEqual(book.issue(4), Decimal("8.00"))
        self.assertEqual(book.qty, 6)


if __name__ == "__main__":
    unittest.main()
