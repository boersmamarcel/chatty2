import datetime
import unittest
from decimal import Decimal

from ledgerly.errors import RateNotFound
from ledgerly.rates import RateTable

D = datetime.date


def table():
    t = RateTable("EUR")
    t.add("USD", D(2024, 1, 1), "0.90")
    t.add("USD", D(2024, 2, 1), "0.92")
    t.add("USD", D(2024, 3, 1), "0.95")
    t.add("GBP", D(2024, 1, 15), "1.15")
    return t


class InclusiveLookupTest(unittest.TestCase):
    def test_rate_effective_on_the_day_applies(self):
        t = table()
        self.assertEqual(t.rate_on("USD", D(2024, 1, 1)), Decimal("0.90"))
        self.assertEqual(t.rate_on("USD", D(2024, 2, 1)), Decimal("0.92"))
        self.assertEqual(t.rate_on("USD", D(2024, 3, 1)), Decimal("0.95"))
        self.assertEqual(t.rate_on("GBP", D(2024, 1, 15)), Decimal("1.15"))

    def test_between_and_after(self):
        t = table()
        self.assertEqual(t.rate_on("USD", D(2024, 1, 31)), Decimal("0.90"))
        self.assertEqual(t.rate_on("USD", D(2024, 2, 29)), Decimal("0.92"))
        self.assertEqual(t.rate_on("USD", D(2025, 6, 1)), Decimal("0.95"))

    def test_convert_on_effective_day(self):
        t = table()
        self.assertEqual(t.convert(Decimal("10.25"), "USD", D(2024, 3, 1)), Decimal("9.74"))
        self.assertEqual(t.convert(Decimal("200"), "GBP", D(2024, 1, 15)), Decimal("230.00"))


class NormalisationTest(unittest.TestCase):
    def test_add_lower_case_with_spaces(self):
        t = RateTable("EUR")
        t.add(" usd ", D(2024, 1, 1), "0.9")
        self.assertEqual(t.rate_on("Usd", D(2024, 1, 1)), Decimal("0.9"))
        self.assertEqual(t.rate_on("USD", D(2024, 1, 2)), Decimal("0.9"))
        self.assertEqual(t.rate_on(" usd", D(2024, 5, 2)), Decimal("0.9"))

    def test_lookup_upper_case_added_as_upper(self):
        t = table()
        self.assertEqual(t.rate_on("gbp", D(2024, 2, 1)), Decimal("1.15"))

    def test_same_day_after_normalisation_replaces(self):
        t = RateTable("EUR")
        t.add("USD", D(2024, 1, 1), "0.90")
        t.add("usd", D(2024, 1, 1), "0.95")
        self.assertEqual(t.rate_on("USD", D(2024, 1, 1)), Decimal("0.95"))
        self.assertEqual(len(t.history("USD")), 1)

    def test_base_currency_any_case(self):
        t = table()
        self.assertEqual(t.rate_on("eur", D(2000, 1, 1)), Decimal("1"))
        self.assertEqual(t.rate_on(" Eur ", D(2024, 1, 1)), Decimal("1"))
        self.assertEqual(t.convert(Decimal("12.34"), "eur", D(2024, 1, 1)), Decimal("12.34"))

    def test_convert_lower_case(self):
        t = table()
        self.assertEqual(t.convert(Decimal("100"), "usd", D(2024, 2, 1)), Decimal("92.00"))


class NotFoundTest(unittest.TestCase):
    def test_unknown_currency_message(self):
        with self.assertRaises(RateNotFound) as ctx:
            table().rate_on("chf", D(2024, 1, 1))
        self.assertEqual(str(ctx.exception), "no CHF rate on or before 2024-01-01")

    def test_before_first_rate_message(self):
        with self.assertRaises(RateNotFound) as ctx:
            table().rate_on("USD", D(2023, 12, 31))
        self.assertEqual(str(ctx.exception), "no USD rate on or before 2023-12-31")

    def test_before_first_rate_normalised_code(self):
        with self.assertRaises(RateNotFound) as ctx:
            table().rate_on(" gbp", D(2024, 1, 14))
        self.assertEqual(str(ctx.exception), "no GBP rate on or before 2024-01-14")


if __name__ == "__main__":
    unittest.main()
