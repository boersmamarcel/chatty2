import datetime
import unittest
from decimal import Decimal

from ledgerly.rates import RateTable

D = datetime.date


class RateTableTest(unittest.TestCase):
    def setUp(self):
        self.table = RateTable("EUR")
        self.table.add("USD", D(2024, 1, 1), "0.90")
        self.table.add("USD", D(2024, 2, 1), "0.92")

    def test_later_rate_takes_over(self):
        self.assertEqual(self.table.rate_on("USD", D(2024, 2, 15)), Decimal("0.92"))
        self.assertEqual(self.table.rate_on("USD", D(2024, 1, 31)), Decimal("0.90"))

    def test_base_currency_is_one(self):
        self.assertEqual(self.table.rate_on("EUR", D(2020, 1, 1)), Decimal("1"))

    def test_rate_applies_on_its_effective_day(self):
        self.assertEqual(self.table.rate_on("USD", D(2024, 2, 1)), Decimal("0.92"))
        self.assertEqual(self.table.rate_on("USD", D(2024, 1, 1)), Decimal("0.90"))

    def test_convert_uses_effective_day_rate(self):
        self.assertEqual(self.table.convert("100", "USD", D(2024, 2, 1)), Decimal("92.00"))


if __name__ == "__main__":
    unittest.main()
