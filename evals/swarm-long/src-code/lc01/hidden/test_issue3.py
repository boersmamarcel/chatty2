import unittest
from decimal import Decimal

from stockroom.valuation import AverageCostBook, ValuationError


class Issue3Test(unittest.TestCase):
    def test_half_up_average(self):
        book = AverageCostBook("S")
        book.receive(32, Decimal("0.03125"))
        self.assertEqual(book.avg_cost, Decimal("0.0313"))
        book = AverageCostBook("S")
        book.receive(8, Decimal("0.15625"))   # value 1.25, 1.25/8 = 0.15625
        self.assertEqual(book.avg_cost, Decimal("0.1563"))
        self.assertIsInstance(book.avg_cost, Decimal)

    def test_average_rounded_after_each_receipt(self):
        book = AverageCostBook("S")
        book.receive(3, Decimal("1.00"))
        self.assertEqual(book.avg_cost, Decimal("1.0000"))
        book.receive(13, Decimal("0.0"))      # 3.00 / 16 = 0.1875
        self.assertEqual(book.avg_cost, Decimal("0.1875"))
        book.receive(16, Decimal("0.03125"))  # (3.00 + 0.50) / 32 = 0.109375
        self.assertEqual(book.avg_cost, Decimal("0.1094"))

    def test_full_issue_takes_remaining_value(self):
        book = AverageCostBook("S")
        book.receive(3000, Decimal("0.033334"))
        self.assertEqual(book.value, Decimal("100.00"))
        self.assertEqual(book.avg_cost, Decimal("0.0333"))
        self.assertEqual(book.issue(1000), Decimal("33.30"))
        self.assertEqual(book.issue(2000), Decimal("66.70"))
        self.assertEqual(book.qty, 0)
        self.assertEqual(book.value, Decimal("0.00"))
        self.assertEqual(book.avg_cost, Decimal("0"))

    def test_receipt_after_stock_out(self):
        book = AverageCostBook("S")
        book.receive(3, Decimal("1.00"))
        book.issue(3)
        book.receive(4, Decimal("2.50"))
        self.assertEqual(book.value, Decimal("10.00"))
        self.assertEqual(book.avg_cost, Decimal("2.5000"))

    def test_over_issue_refused_and_book_unchanged(self):
        book = AverageCostBook("S")
        book.receive(10, Decimal("1.25"))
        book.issue(4)
        with self.assertRaises(ValuationError):
            book.issue(7)
        self.assertEqual(book.qty, 6)
        self.assertEqual(book.value, Decimal("7.50"))
        self.assertEqual(book.avg_cost, Decimal("1.2500"))
        self.assertEqual(book.issue(6), Decimal("7.50"))


if __name__ == "__main__":
    unittest.main()
