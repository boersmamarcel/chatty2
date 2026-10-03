import datetime
import unittest

from gradebook.late import apply_late, days_late

DUE = datetime.datetime(2026, 3, 2, 23, 59)


def after(**kwargs):
    return DUE + datetime.timedelta(**kwargs)


class DaysLateTest(unittest.TestCase):
    def test_on_time(self):
        self.assertEqual(days_late(DUE, DUE), 0)
        self.assertEqual(days_late(DUE, after(hours=-3)), 0)

    def test_part_of_a_day_counts(self):
        self.assertEqual(days_late(DUE, after(minutes=1)), 1)
        self.assertEqual(days_late(DUE, after(days=1, minutes=1)), 2)


class ApplyLateTest(unittest.TestCase):
    def test_penalty_is_share_of_maximum(self):
        self.assertAlmostEqual(apply_late(15.0, 20.0, DUE, after(hours=1)), 13.0)

    def test_on_time_unchanged(self):
        self.assertEqual(apply_late(15.0, 20.0, DUE, DUE), 15.0)


if __name__ == "__main__":
    unittest.main()
