import datetime
import unittest

from gradebook.late import apply_late, days_late
from gradebook.models import EXCUSED

DUE = datetime.datetime(2026, 3, 2, 23, 59)


def after(**kwargs):
    return DUE + datetime.timedelta(**kwargs)


class DaysLateTest(unittest.TestCase):
    def test_counts(self):
        self.assertEqual(days_late(DUE, after(minutes=1)), 1)
        self.assertEqual(days_late(DUE, after(hours=2)), 1)
        self.assertEqual(days_late(DUE, after(days=1)), 1)
        self.assertEqual(days_late(DUE, after(days=1, minutes=1)), 2)
        self.assertEqual(days_late(DUE, after(days=3, hours=1)), 4)
        self.assertEqual(days_late(DUE, DUE), 0)
        self.assertEqual(days_late(DUE, after(days=-2)), 0)
        self.assertEqual(days_late(None, after(days=4)), 0)
        self.assertEqual(days_late(DUE, None), 0)


class ApplyLateTest(unittest.TestCase):
    def test_examples(self):
        self.assertAlmostEqual(apply_late(15.0, 20.0, DUE, after(hours=1)), 13.0)
        self.assertAlmostEqual(apply_late(15.0, 20.0, DUE, after(days=3, hours=1)), 7.0)
        self.assertAlmostEqual(apply_late(15.0, 20.0, DUE, after(days=9)), 5.0)
        self.assertAlmostEqual(apply_late(15.0, 20.0, DUE, after(days=5)), 5.0)
        self.assertEqual(apply_late(2.0, 20.0, DUE, after(days=2)), 0.0)
        self.assertEqual(apply_late(15.0, 20.0, DUE, DUE), 15.0)

    def test_extension(self):
        self.assertAlmostEqual(apply_late(15.0, 20.0, DUE, after(hours=25), extension_days=1), 13.0)
        self.assertEqual(apply_late(15.0, 20.0, DUE, after(hours=47), extension_days=2), 15.0)
        self.assertAlmostEqual(apply_late(15.0, 20.0, DUE, after(days=4), extension_days=1), 9.0)

    def test_extra_credit_and_markers(self):
        self.assertAlmostEqual(apply_late(22.0, 20.0, DUE, after(hours=1)), 20.0)
        self.assertIsNone(apply_late(None, 20.0, DUE, after(days=2)))
        self.assertIs(apply_late(EXCUSED, 20.0, DUE, after(days=2)), EXCUSED)

    def test_no_due(self):
        self.assertEqual(apply_late(15.0, 20.0, None, after(days=2)), 15.0)


if __name__ == "__main__":
    unittest.main()
