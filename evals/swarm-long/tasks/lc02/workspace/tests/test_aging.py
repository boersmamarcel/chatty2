import datetime
import unittest
from decimal import Decimal

from ledgerly.aging import OpenItem, aging_report, bucket_for, days_overdue

D = datetime.date


class BucketTest(unittest.TestCase):
    def test_current(self):
        self.assertEqual(bucket_for(-5), "current")
        self.assertEqual(bucket_for(0), "current")

    def test_upper_bounds_are_inclusive(self):
        self.assertEqual(bucket_for(1), "1-30")
        self.assertEqual(bucket_for(30), "1-30")
        self.assertEqual(bucket_for(31), "31-60")
        self.assertEqual(bucket_for(60), "31-60")
        self.assertEqual(bucket_for(90), "61-90")
        self.assertEqual(bucket_for(91), "90+")


class DaysOverdueTest(unittest.TestCase):
    def test_counted_from_due_date(self):
        item = OpenItem("INV-1", "Acme", D(2024, 1, 1), D(2024, 1, 31), "100")
        self.assertEqual(days_overdue(item, D(2024, 2, 10)), 10)
        self.assertEqual(days_overdue(item, D(2024, 1, 20)), -11)

    def test_report_uses_due_date(self):
        item = OpenItem("INV-1", "Acme", D(2024, 1, 1), D(2024, 1, 31), "100")
        report = aging_report([item], [], D(2024, 2, 10))
        self.assertEqual(report.customer("Acme")["1-30"], Decimal("100"))


if __name__ == "__main__":
    unittest.main()
