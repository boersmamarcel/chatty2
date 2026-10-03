import datetime
import unittest
from decimal import Decimal

from ledgerly.accounts import default_chart
from ledgerly.errors import UnbalancedEntryError
from ledgerly.fx import convert_amount, convert_entry
from ledgerly.journal import JournalEntry
from ledgerly.ledger import Ledger
from ledgerly.rates import RateTable

D = datetime.date
X = Decimal


def rates():
    table = RateTable("EUR")
    table.add("USD", D(2024, 1, 1), "0.92")
    table.add("GBP", D(2024, 1, 1), "1.17")
    table.add("CHF", D(2024, 1, 1), "0.335")
    table.add("SEK", D(2024, 1, 1), "0.5")
    return table


def lines(entry):
    return [(l.account, l.side, l.amount, l.currency, l.base_amount, l.memo) for l in entry.lines]


class BaseTotalsTest(unittest.TestCase):
    def test_uses_base_amounts(self):
        entry = JournalEntry(D(2024, 2, 1), "x").debit("1100", "100.00", "USD").credit("4000", "92.00")
        entry.lines[0].base_amount = X("92.00")
        entry.lines[1].base_amount = X("92.00")
        self.assertEqual(entry.base_totals(), (X("92.00"), X("92.00")))
        self.assertTrue(entry.is_balanced())
        entry.validate(default_chart())

    def test_issue_example_posts(self):
        ledger = Ledger(default_chart())
        entry = JournalEntry(D(2024, 2, 1), "usd sale").debit("1100", "100.00", "USD").credit("4000", "92.00")
        ledger.post_foreign(entry, rates(), rounding_account="6900")
        self.assertEqual(len(entry.lines), 2)
        self.assertEqual(ledger.balance("1100"), X("92.00"))
        self.assertEqual(ledger.balance("4000"), X("92.00"))


class ConvertAmountTest(unittest.TestCase):
    def test_half_up(self):
        self.assertEqual(convert_amount(X("100.25"), X("0.5"), "EUR"), X("50.13"))
        self.assertEqual(convert_amount(X("2.5"), X("0.5"), "EUR"), X("1.25"))
        self.assertEqual(convert_amount(X("0.01"), X("0.5"), "EUR"), X("0.01"))
        self.assertEqual(str(convert_amount(X("10"), X("0.92"), "EUR")), "9.20")

    def test_base_minor_units(self):
        self.assertEqual(convert_amount(X("12.5"), X("1"), "JPY"), X("13"))
        self.assertEqual(convert_amount(X("0.0025"), X("1"), "BHD"), X("0.003"))


class ConvertEntryTest(unittest.TestCase):
    def test_half_up_lines_balance_without_rounding_line(self):
        entry = JournalEntry(D(2024, 3, 2), "sek").debit("1100", "100.25", "SEK").credit("4000", "50.13")
        convert_entry(entry, rates(), rounding_account="6900")
        self.assertEqual(lines(entry), [
            ("1100", "D", X("100.25"), "SEK", X("50.13"), ""),
            ("4000", "C", X("50.13"), None, X("50.13"), ""),
        ])

    def _chf_entry(self, credit):
        entry = JournalEntry(D(2024, 2, 1), "chf")
        for _ in range(3):
            entry.debit("1100", "1.00", "CHF")
        entry.credit("4000", credit)
        return entry

    def test_rounding_line_at_the_limit_credit(self):
        # 3 x round(0.335) = 3 x 0.34 = 1.02 vs 0.99: d = 0.03 = 3 x 0.01
        entry = convert_entry(self._chf_entry("0.99"), rates(), rounding_account="6900")
        self.assertEqual(len(entry.lines), 5)
        self.assertEqual(lines(entry)[-1], ("6900", "C", X("0.03"), None, X("0.03"), "FX rounding"))
        self.assertTrue(entry.is_balanced())

    def test_rounding_line_debit(self):
        entry = convert_entry(self._chf_entry("1.05"), rates(), rounding_account="6900")
        self.assertEqual(lines(entry)[-1], ("6900", "D", X("0.03"), None, X("0.03"), "FX rounding"))
        ledger = Ledger(default_chart())
        ledger.post(entry)
        self.assertEqual(ledger.balance("6900"), X("0.03"))

    def test_over_the_limit_raises(self):
        with self.assertRaises(UnbalancedEntryError) as ctx:
            convert_entry(self._chf_entry("0.98"), rates(), rounding_account="6900")
        self.assertEqual(str(ctx.exception), "entry unbalanced by 0.04 EUR after conversion")

    def test_over_the_limit_negative_has_no_sign(self):
        with self.assertRaises(UnbalancedEntryError) as ctx:
            convert_entry(self._chf_entry("1.06"), rates(), rounding_account="6900")
        self.assertEqual(str(ctx.exception), "entry unbalanced by 0.04 EUR after conversion")

    def test_no_rounding_account(self):
        with self.assertRaises(UnbalancedEntryError) as ctx:
            convert_entry(self._chf_entry("1.03"), rates())
        self.assertEqual(str(ctx.exception), "entry unbalanced by 0.01 EUR after conversion")

    def test_single_foreign_line_limit(self):
        # GBP 0.01 * 1.17 = 0.0117 -> 0.01 ; credit 0.02 -> d = -0.01 (allowed for one line)
        entry = JournalEntry(D(2024, 2, 1), "gbp").debit("1100", "0.01", "GBP").credit("4000", "0.02")
        convert_entry(entry, rates(), rounding_account="6900")
        self.assertEqual(lines(entry)[-1], ("6900", "D", X("0.01"), None, X("0.01"), "FX rounding"))
        entry = JournalEntry(D(2024, 2, 1), "gbp").debit("1100", "0.01", "GBP").credit("4000", "0.03")
        with self.assertRaises(UnbalancedEntryError):
            convert_entry(entry, rates(), rounding_account="6900")

    def test_foreign_on_both_sides(self):
        entry = (JournalEntry(D(2024, 2, 1), "swap")
                 .debit("1100", "100.00", "GBP")
                 .credit("1000", "127.17", "USD"))
        ledger = Ledger(default_chart())
        ledger.post_foreign(entry, rates(), rounding_account="6900")
        # 100 GBP = 117.00 EUR; 127.17 USD = 116.9964 -> 117.00 EUR
        self.assertEqual([l.base_amount for l in entry.lines], [X("117.00"), X("117.00")])
        self.assertEqual(ledger.balance("1000"), X("-117.00"))


if __name__ == "__main__":
    unittest.main()
