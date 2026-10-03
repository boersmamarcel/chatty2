import datetime
import unittest
from decimal import Decimal

from ledgerly import trial_balance
from ledgerly.accounts import ASSET, EQUITY, EXPENSE, INCOME, ChartOfAccounts
from ledgerly.journal import JournalEntry
from ledgerly.ledger import Ledger

D = datetime.date


def make_ledger():
    chart = ChartOfAccounts()
    chart.create("900", "Petty cash", ASSET)
    chart.create("1000", "Cash", ASSET)
    chart.create("1100", "Bank", ASSET)
    chart.create("3000", "Share capital", EQUITY)
    chart.create("4000", "Sales", INCOME)
    chart.create("6000", "Rent", EXPENSE)
    ledger = Ledger(chart)
    ledger.post(JournalEntry(D(2024, 1, 2), "capital").debit("1100", "500.00").credit("3000", "500.00"))
    ledger.post(JournalEntry(D(2024, 1, 3), "float").debit("900", "50.00").credit("1100", "50.00"))
    ledger.post(JournalEntry(D(2024, 1, 5), "sale").debit("1000", "120.00").credit("4000", "120.00"))
    ledger.post(JournalEntry(D(2024, 1, 6), "deposit").debit("1100", "120.00").credit("1000", "120.00"))
    ledger.post(JournalEntry(D(2024, 1, 9), "rent").debit("6000", "80.00").credit("1100", "80.00"))
    return ledger


class TrialBalanceTest(unittest.TestCase):
    def test_columns_balance(self):
        rows = trial_balance.build(make_ledger())
        self.assertTrue(trial_balance.is_balanced(rows))

    def test_rows_in_numeric_code_order(self):
        rows = trial_balance.build(make_ledger())
        self.assertEqual([r.code for r in rows], ["900", "1100", "3000", "4000", "6000"])

    def test_zero_balance_accounts_are_left_out(self):
        rows = trial_balance.build(make_ledger())
        self.assertNotIn("1000", [r.code for r in rows])

    def test_amounts(self):
        rows = dict((r.code, (r.debit, r.credit)) for r in trial_balance.build(make_ledger()))
        self.assertEqual(rows["1100"], (Decimal("490.00"), Decimal("0")))
        self.assertEqual(rows["4000"], (Decimal("0"), Decimal("120.00")))


if __name__ == "__main__":
    unittest.main()
