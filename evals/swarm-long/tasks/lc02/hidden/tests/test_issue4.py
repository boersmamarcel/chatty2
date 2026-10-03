import datetime
import unittest
from decimal import Decimal

from ledgerly.accounts import INCOME, default_chart
from ledgerly.errors import PeriodLockedError
from ledgerly.journal import JournalEntry
from ledgerly.ledger import Ledger
from ledgerly.periods import FiscalCalendar, Period, close_period

D = datetime.date
X = Decimal


def make_ledger():
    chart = default_chart()
    chart.create("950", "Other income", INCOME)
    ledger = Ledger(chart, calendar=FiscalCalendar())
    post = ledger.post
    post(JournalEntry(D(2024, 2, 29), "feb sale").debit("1100", "50.00").credit("4000", "50.00"))
    post(JournalEntry(D(2024, 3, 1), "sale").debit("1100", "500.00").credit("4000", "500.00"))
    post(JournalEntry(D(2024, 3, 15), "rent").debit("6000", "300.00").credit("1100", "300.00"))
    post(JournalEntry(D(2024, 3, 20), "salary").debit("6100", "150.00").credit("1100", "150.00"))
    post(JournalEntry(D(2024, 3, 25), "salary refund").debit("1100", "150.00").credit("6100", "150.00"))
    post(JournalEntry(D(2024, 3, 31), "service").debit("1200", "200.00").credit("4100", "200.00"))
    post(JournalEntry(D(2024, 3, 31), "interest").debit("1100", "7.50").credit("950", "7.50"))
    post(JournalEntry(D(2024, 4, 1), "april sale").debit("1100", "999.00").credit("4000", "999.00"))
    return ledger


def lines(entry):
    return [(l.account, l.side, l.amount) for l in entry.lines]


class ContainsTest(unittest.TestCase):
    def test_inclusive_both_ends(self):
        self.assertTrue(Period(2024, 2).contains(D(2024, 2, 29)))
        self.assertTrue(Period(2024, 2).contains(D(2024, 2, 1)))
        self.assertTrue(Period(2023, 12).contains(D(2023, 12, 31)))
        self.assertFalse(Period(2024, 2).contains(D(2024, 3, 1)))
        self.assertFalse(Period(2024, 2).contains(D(2024, 1, 31)))


class CloseTest(unittest.TestCase):
    def test_closing_entry(self):
        ledger = make_ledger()
        closing = close_period(ledger, Period(2024, 3))
        self.assertIsNotNone(closing.entry_id)
        self.assertEqual(closing.date, D(2024, 3, 31))
        self.assertEqual(closing.description, "Close 2024-03")
        self.assertEqual(lines(closing), [
            ("950", "D", X("7.50")),
            ("4000", "D", X("500.00")),
            ("4100", "D", X("200.00")),
            ("6000", "C", X("300.00")),
            ("3100", "C", X("407.50")),
        ])

    def test_balances_after_close(self):
        ledger = make_ledger()
        close_period(ledger, Period(2024, 3))
        self.assertEqual(ledger.balance("3100"), X("407.50"))
        self.assertEqual(ledger.balance("4100"), X("0"))
        self.assertEqual(ledger.balance("950"), X("0"))
        self.assertEqual(ledger.balance("4000", as_of=D(2024, 3, 31)), X("50.00"))
        self.assertEqual(ledger.balance("4000"), X("1049.00"))

    def test_posting_into_closed_period_rejected(self):
        ledger = make_ledger()
        close_period(ledger, Period(2024, 3))
        for day in (D(2024, 3, 1), D(2024, 3, 17), D(2024, 3, 31)):
            entry = JournalEntry(day, "late").debit("1100", "1.00").credit("4000", "1.00")
            with self.assertRaises(PeriodLockedError) as ctx:
                ledger.post(entry)
            self.assertEqual(str(ctx.exception), "period 2024-03 is closed")
        ledger.post(JournalEntry(D(2024, 4, 1), "ok").debit("1100", "1.00").credit("4000", "1.00"))
        ledger.post(JournalEntry(D(2024, 2, 29), "ok").debit("1100", "1.00").credit("4000", "1.00"))

    def test_close_twice(self):
        ledger = make_ledger()
        close_period(ledger, Period(2024, 3))
        with self.assertRaises(PeriodLockedError) as ctx:
            close_period(ledger, Period(2024, 3))
        self.assertEqual(str(ctx.exception), "period 2024-03 is closed")

    def test_loss_debits_retained_earnings(self):
        ledger = Ledger(default_chart(), calendar=FiscalCalendar())
        ledger.post(JournalEntry(D(2024, 2, 10), "sale").debit("1100", "100.00").credit("4000", "100.00"))
        ledger.post(JournalEntry(D(2024, 2, 29), "rent").debit("6000", "250.00").credit("1100", "250.00"))
        closing = close_period(ledger, Period(2024, 2))
        self.assertEqual(closing.date, D(2024, 2, 29))
        self.assertEqual(lines(closing), [
            ("4000", "D", X("100.00")),
            ("6000", "C", X("250.00")),
            ("3100", "D", X("150.00")),
        ])

    def test_zero_result_has_no_retained_earnings_line(self):
        ledger = Ledger(default_chart(), calendar=FiscalCalendar())
        ledger.post(JournalEntry(D(2024, 5, 2), "sale").debit("1100", "80.00").credit("4000", "80.00"))
        ledger.post(JournalEntry(D(2024, 5, 31), "rent").debit("6000", "80.00").credit("1100", "80.00"))
        closing = close_period(ledger, Period(2024, 5), retained_earnings="3100")
        self.assertEqual(lines(closing), [("4000", "D", X("80.00")), ("6000", "C", X("80.00"))])

    def test_nothing_to_close(self):
        ledger = Ledger(default_chart(), calendar=FiscalCalendar())
        ledger.post(JournalEntry(D(2024, 6, 3), "capital").debit("1100", "1000").credit("3000", "1000"))
        ledger.post(JournalEntry(D(2024, 6, 4), "sale").debit("1100", "40").credit("4000", "40"))
        ledger.post(JournalEntry(D(2024, 6, 30), "reversal").debit("4000", "40").credit("1100", "40"))
        count = len(ledger)
        self.assertIsNone(close_period(ledger, Period(2024, 6)))
        self.assertEqual(len(ledger), count)
        with self.assertRaises(PeriodLockedError) as ctx:
            ledger.post(JournalEntry(D(2024, 6, 30), "late").debit("1100", "1").credit("3000", "1"))
        self.assertEqual(str(ctx.exception), "period 2024-06 is closed")

    def test_empty_period(self):
        ledger = Ledger(default_chart(), calendar=FiscalCalendar())
        self.assertIsNone(close_period(ledger, Period(2024, 7)))
        self.assertTrue(ledger.calendar.is_locked(D(2024, 7, 31)))


if __name__ == "__main__":
    unittest.main()
