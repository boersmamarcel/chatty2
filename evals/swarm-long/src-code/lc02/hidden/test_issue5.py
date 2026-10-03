import datetime
import unittest
from decimal import Decimal

from ledgerly.aging import OpenItem, Payment, aging_report, allocate_payments, bucket_for, days_overdue

D = datetime.date
X = Decimal


class BucketTest(unittest.TestCase):
    def test_every_boundary(self):
        expected = {
            -30: "current", -1: "current", 0: "current",
            1: "1-30", 15: "1-30", 30: "1-30",
            31: "31-60", 59: "31-60", 60: "31-60",
            61: "61-90", 89: "61-90", 90: "61-90",
            91: "90+", 365: "90+",
        }
        for days, bucket in sorted(expected.items()):
            self.assertEqual(bucket_for(days), bucket, days)

    def test_days_from_due_date(self):
        item = OpenItem("INV-1", "Acme", D(2023, 12, 1), D(2024, 1, 31), "10")
        self.assertEqual(days_overdue(item, D(2024, 1, 31)), 0)
        self.assertEqual(days_overdue(item, D(2024, 3, 1)), 30)
        self.assertEqual(days_overdue(item, D(2024, 1, 1)), -30)


def acme_items():
    return [
        OpenItem("INV-2", "Acme", D(2024, 1, 1), D(2024, 3, 1), "100.00"),
        OpenItem("INV-3", "Acme", D(2024, 1, 5), D(2024, 2, 10), "100.00"),
        OpenItem("INV-1", "Acme", D(2024, 1, 10), D(2024, 2, 10), "100.00"),
    ]


class AllocationTest(unittest.TestCase):
    def test_due_date_then_number(self):
        remaining = allocate_payments(acme_items(), [Payment("Acme", D(2024, 2, 1), "150.00")])
        self.assertEqual(remaining, {"INV-1": X("0.00"), "INV-3": X("50.00"), "INV-2": X("100.00")})

    def test_number_tie_break_is_string_order(self):
        items = [
            OpenItem("INV-9", "Acme", D(2024, 1, 1), D(2024, 2, 1), "40"),
            OpenItem("INV-10", "Acme", D(2024, 1, 2), D(2024, 2, 1), "40"),
        ]
        remaining = allocate_payments(items, [Payment("Acme", D(2024, 1, 20), "50")])
        self.assertEqual(remaining, {"INV-10": X("0"), "INV-9": X("30")})

    def test_payments_after_as_of_ignored(self):
        payments = [
            Payment("Acme", D(2024, 2, 1), "60.00"),
            Payment("Acme", D(2024, 3, 15), "40.00"),
            Payment("Acme", D(2024, 3, 16), "200.00"),
        ]
        remaining = allocate_payments(acme_items(), payments, D(2024, 3, 15))
        self.assertEqual(remaining, {"INV-1": X("0.00"), "INV-3": X("100.00"), "INV-2": X("100.00")})
        remaining = allocate_payments(acme_items(), payments)
        self.assertEqual(remaining, {"INV-1": X("0.00"), "INV-3": X("0.00"), "INV-2": X("0.00")})

    def test_payments_stay_with_customer(self):
        items = acme_items() + [OpenItem("INV-0", "Beta", D(2023, 1, 1), D(2023, 1, 31), "70")]
        remaining = allocate_payments(items, [Payment("Beta", D(2024, 1, 1), "100")])
        self.assertEqual(remaining["INV-0"], X("0"))
        self.assertEqual(remaining["INV-1"], X("100.00"))


class ReportTest(unittest.TestCase):
    def test_report(self):
        items = acme_items() + [
            OpenItem("INV-5", "Beta", D(2023, 10, 1), D(2023, 10, 31), "70.00"),
            OpenItem("INV-6", "Beta", D(2024, 3, 1), D(2024, 4, 1), "20.00"),
            OpenItem("INV-4", "Able", D(2024, 1, 1), D(2024, 1, 15), "30.00"),
        ]
        payments = [
            Payment("Acme", D(2024, 2, 1), "120.00"),
            Payment("Acme", D(2024, 4, 2), "80.00"),
            Payment("Able", D(2024, 4, 1), "30.00"),
            Payment("Beta", D(2024, 4, 30), "5.00"),
        ]
        report = aging_report(items, payments, D(2024, 4, 1))
        self.assertEqual([c for c, _b in report.rows], ["Acme", "Beta"])
        acme = report.customer("Acme")
        # INV-1 paid, INV-3 (due 2/10, 51 days) has 80 left, INV-2 (due 3/1, 31 days) 100
        self.assertEqual(acme["31-60"], X("180.00"))
        self.assertEqual(acme["1-30"], X("0"))
        beta = report.customer("Beta")
        self.assertEqual(beta["90+"], X("70.00"))
        self.assertEqual(beta["61-90"], X("0"))
        self.assertEqual(beta["current"], X("20.00"))
        # INV-5 due 2023-10-31 is 153 days overdue on 2024-04-01
        self.assertEqual(report.totals()["90+"], X("70.00"))
        self.assertEqual(report.total(), X("270.00"))
        self.assertEqual(sorted(i.number for i, _r, _b in report.items), ["INV-2", "INV-3", "INV-5", "INV-6"])


if __name__ == "__main__":
    unittest.main()
