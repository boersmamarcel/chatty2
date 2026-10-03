import unittest

from splitbill.currency import CurrencyError, convert, parse_rates
from splitbill.importer import load_expenses


class ConvertTest(unittest.TestCase):
    def test_direction_and_rounding(self):
        rates = parse_rates("USD 1.0850\nSEK 10\n")
        self.assertEqual(convert(10000, "USD", "EUR", rates), 9217)
        self.assertEqual(convert(10000, "EUR", "USD", rates), 10850)
        self.assertEqual(convert(25, "SEK", "EUR", rates), 3)
        self.assertEqual(convert(-25, "SEK", "EUR", rates), -3)
        self.assertEqual(convert(24, "SEK", "EUR", rates), 2)

    def test_cross(self):
        rates = parse_rates("USD 1.25\nGBP 0.86\n")
        self.assertEqual(convert(1000, "USD", "GBP", rates), 688)
        self.assertEqual(convert(1001, "USD", "EUR", rates), 801)
        self.assertIsInstance(convert(1001, "USD", "EUR", rates), int)

    def test_base_not_in_rates(self):
        rates = {"USD": parse_rates("USD 1.25")["USD"]}
        self.assertEqual(convert(500, "eur", "usd", rates), 625)

    def test_codes(self):
        rates = parse_rates("USD 1.25\n")
        self.assertEqual(convert(1000, "usd", "Eur", rates), 800)
        self.assertEqual(convert(1234, "usd", "USD", rates), 1234)
        with self.assertRaises(CurrencyError):
            convert(100, "JPY", "EUR", rates)
        with self.assertRaises(CurrencyError):
            convert(100, "EUR", "jpy", rates)
        with self.assertRaises(CurrencyError):
            convert(100, "JPY", "JPY", rates)

    def test_import_converts(self):
        text = ("date,payer,amount,currency,description,split\n"
                "2026-05-01,anna,100.00,USD,dinner,\"exact:anna=50.00,ben=42.17\"\n")
        rates = parse_rates("USD 1.0850\n")
        expenses = load_expenses(text, "EUR", rates)
        self.assertEqual(expenses[0].amount, 9217)


if __name__ == "__main__":
    unittest.main()
