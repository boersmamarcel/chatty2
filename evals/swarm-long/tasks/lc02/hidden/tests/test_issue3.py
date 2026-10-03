import datetime
import unittest
from decimal import Decimal

from ledgerly import trial_balance
from ledgerly.accounts import ASSET, EQUITY, EXPENSE, INCOME, LIABILITY, ChartOfAccounts
from ledgerly.formatting import format_amount
from ledgerly.journal import JournalEntry
from ledgerly.ledger import Ledger

D = datetime.date
X = Decimal


def make_ledger():
    chart = ChartOfAccounts()
    chart.create("6000", "Rent", EXPENSE)
    chart.create("1000", "Cash", ASSET)
    chart.create("900", "Petty cash", ASSET)
    chart.create("3000", "Share capital", EQUITY)
    chart.create("1100", "Bank", ASSET)
    chart.create("2000", "Accounts payable", LIABILITY)
    chart.create("4000", "Sales", INCOME)
    ledger = Ledger(chart)
    ledger.post(JournalEntry(D(2024, 1, 2), "c").debit("1100", "25000.00").credit("3000", "25000.00"))
    ledger.post(JournalEntry(D(2024, 1, 3), "f").debit("900", "150.00").credit("1100", "150.00"))
    ledger.post(JournalEntry(D(2024, 1, 5), "s").debit("1000", "12500.00").credit("4000", "12500.00"))
    ledger.post(JournalEntry(D(2024, 1, 6), "d").debit("1100", "12500.00").credit("1000", "12500.00"))
    ledger.post(JournalEntry(D(2024, 1, 31), "r").debit("6000", "2000.00").credit("1100", "2000.00"))
    return ledger


class BuildTest(unittest.TestCase):
    def test_zero_rows_left_out_and_numeric_order(self):
        rows = trial_balance.build(make_ledger())
        self.assertEqual([r.as_tuple() for r in rows], [
            ("900", "Petty cash", X("150.00"), X("0")),
            ("1100", "Bank", X("35350.00"), X("0")),
            ("3000", "Share capital", X("0"), X("25000.00")),
            ("4000", "Sales", X("0"), X("12500.00")),
            ("6000", "Rent", X("2000.00"), X("0")),
        ])

    def test_as_of_is_inclusive(self):
        rows = trial_balance.build(make_ledger(), as_of=D(2024, 1, 5))
        self.assertEqual([r.code for r in rows], ["900", "1000", "1100", "3000", "4000"])
        rows = trial_balance.build(make_ledger(), as_of=D(2024, 1, 6))
        self.assertEqual([r.code for r in rows], ["900", "1100", "3000", "4000"])

    def test_include_zero_lists_every_account(self):
        rows = trial_balance.build(make_ledger(), as_of=D(2024, 1, 30), include_zero=True)
        self.assertEqual([r.code for r in rows], ["900", "1000", "1100", "2000", "3000", "4000", "6000"])
        by_code = dict((r.code, (r.debit, r.credit)) for r in rows)
        self.assertEqual(by_code["1000"], (X("0"), X("0")))
        self.assertEqual(by_code["2000"], (X("0"), X("0")))
        self.assertEqual(by_code["6000"], (X("0"), X("0")))
        self.assertTrue(trial_balance.is_balanced(rows))

    def test_empty_ledger(self):
        ledger = Ledger(make_ledger().chart)
        self.assertEqual(trial_balance.build(ledger), [])
        self.assertEqual(len(trial_balance.build(ledger, include_zero=True)), 7)


class FormatAmountTest(unittest.TestCase):
    def test_examples(self):
        self.assertEqual(format_amount(X("1234567.5")), "1,234,567.50")
        self.assertEqual(format_amount(X("-1234.5")), "-1,234.50")
        self.assertEqual(format_amount(X("999.995")), "1,000.00")
        self.assertEqual(format_amount(X("1234.5"), "JPY"), "1,235")
        self.assertEqual(format_amount(X("0")), "0.00")

    def test_small_amounts_unchanged(self):
        self.assertEqual(format_amount(X("999.99")), "999.99")
        self.assertEqual(format_amount(X("-0.005")), "-0.01")
        self.assertEqual(format_amount(X("12.3456"), "BHD"), "12.346")
        self.assertEqual(format_amount(X("1000000"), "BHD"), "1,000,000.000")


class RenderTest(unittest.TestCase):
    def test_render(self):
        expected = "\n".join([
            "Code  Account            Debit     Credit",
            "-----------------------------------------",
            "900   Petty cash        150.00",
            "1100  Bank           35,350.00",
            "3000  Share capital             25,000.00",
            "4000  Sales                     12,500.00",
            "6000  Rent            2,000.00",
            "      TOTAL          37,500.00  37,500.00",
        ])
        self.assertEqual(trial_balance.render(trial_balance.build(make_ledger())), expected)


if __name__ == "__main__":
    unittest.main()
