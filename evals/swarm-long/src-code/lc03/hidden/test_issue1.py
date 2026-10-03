import unittest
from datetime import datetime

from rota.recurrence import Recurrence


def d(day, hour=9, month=3):
    return datetime(2026, month, day, hour, 0)


class UntilInclusiveTest(unittest.TestCase):

    def test_iter_until_inclusive(self):
        rule = Recurrence.parse("FREQ=DAILY;UNTIL=20260305T090000")
        self.assertEqual(list(rule.iter_occurrences(d(1))), [d(1), d(2), d(3), d(4), d(5)])

    def test_until_one_second_before(self):
        rule = Recurrence.parse("FREQ=DAILY;UNTIL=20260305T085959")
        self.assertEqual(list(rule.iter_occurrences(d(1)))[-1], d(4))

    def test_between_until_inclusive(self):
        rule = Recurrence.parse("FREQ=WEEKLY;BYDAY=WE;UNTIL=20260318T090000")
        self.assertEqual(rule.between(d(4), d(10, 0), d(1, 0, month=4)), [d(11), d(18)])


class NothingBeforeDtstartTest(unittest.TestCase):

    def test_byday_skips_days_before_dtstart(self):
        # 2026-03-04 is a Wednesday
        rule = Recurrence.parse("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=3")
        self.assertEqual(list(rule.iter_occurrences(d(4))), [d(4), d(9), d(11)])

    def test_byday_with_interval(self):
        # 2026-03-06 is a Friday
        rule = Recurrence.parse("FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,FR;COUNT=3")
        self.assertEqual(list(rule.iter_occurrences(d(6))), [d(6), d(16), d(20)])

    def test_between_never_before_dtstart(self):
        rule = Recurrence.parse("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=2")
        self.assertEqual(rule.between(d(4), d(1, 0), d(31, 0)), [d(4), d(9)])

    def test_until_with_byday(self):
        rule = Recurrence.parse("FREQ=WEEKLY;BYDAY=MO,WE,FR;UNTIL=20260309T090000")
        self.assertEqual(list(rule.iter_occurrences(d(4))), [d(4), d(6), d(9)])


class CountBoundsSeriesTest(unittest.TestCase):

    def test_window_after_start(self):
        rule = Recurrence.parse("FREQ=DAILY;COUNT=5")
        self.assertEqual(rule.between(d(1), d(3, 0), d(10, 0)), [d(3), d(4), d(5)])

    def test_window_at_start(self):
        rule = Recurrence.parse("FREQ=DAILY;COUNT=5")
        self.assertEqual(rule.between(d(1), d(1, 0), d(3, 0)), [d(1), d(2)])

    def test_window_after_series_end(self):
        rule = Recurrence.parse("FREQ=DAILY;COUNT=5")
        self.assertEqual(rule.between(d(1), d(6, 0), d(20, 0)), [])

    def test_weekly_count_and_window(self):
        rule = Recurrence.parse("FREQ=WEEKLY;BYDAY=MO,WE;COUNT=4")
        # series: Mar 4, 9, 11, 16
        self.assertEqual(rule.between(d(4), d(10, 0), d(1, 0, month=4)), [d(11), d(16)])

    def test_window_end_exclusive(self):
        rule = Recurrence.parse("FREQ=DAILY;COUNT=5")
        self.assertEqual(rule.between(d(1), d(2), d(4)), [d(2), d(3)])


if __name__ == "__main__":
    unittest.main()
