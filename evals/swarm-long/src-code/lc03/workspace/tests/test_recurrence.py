import unittest
from datetime import datetime

from rota.recurrence import Recurrence


class RecurrenceTest(unittest.TestCase):

    def test_parse_and_canonical_text(self):
        rule = Recurrence.parse("rrule:freq=weekly;byday=we,mo;interval=2;count=4")
        self.assertEqual(rule.to_string(), "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=4")

    def test_daily_interval(self):
        rule = Recurrence.parse("FREQ=DAILY;INTERVAL=3;COUNT=3")
        got = list(rule.iter_occurrences(datetime(2026, 3, 1, 9, 0)))
        self.assertEqual(got, [datetime(2026, 3, 1, 9), datetime(2026, 3, 4, 9),
                               datetime(2026, 3, 7, 9)])

    def test_monthly_skips_short_months(self):
        rule = Recurrence.parse("FREQ=MONTHLY;COUNT=3")
        got = list(rule.iter_occurrences(datetime(2026, 1, 31, 10, 0)))
        self.assertEqual(got, [datetime(2026, 1, 31, 10), datetime(2026, 3, 31, 10),
                               datetime(2026, 5, 31, 10)])

    def test_until_is_inclusive(self):
        rule = Recurrence.parse("FREQ=DAILY;UNTIL=20260305T090000")
        got = list(rule.iter_occurrences(datetime(2026, 3, 1, 9, 0)))
        self.assertEqual(len(got), 5)
        self.assertEqual(got[-1], datetime(2026, 3, 5, 9, 0))


if __name__ == "__main__":
    unittest.main()
