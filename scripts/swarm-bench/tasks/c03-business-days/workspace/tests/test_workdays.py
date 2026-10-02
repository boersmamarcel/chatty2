import datetime
import unittest

from workdays import business_days

D = datetime.date


class BusinessDaysTest(unittest.TestCase):
    def test_one_week(self):
        self.assertEqual(business_days(D(2026, 9, 7), D(2026, 9, 13)), 5)

    def test_weekday_holiday(self):
        self.assertEqual(business_days(D(2026, 12, 21), D(2026, 12, 27), [D(2026, 12, 25)]), 4)

    def test_holiday_on_a_saturday_changes_nothing(self):
        self.assertEqual(business_days(D(2026, 12, 21), D(2026, 12, 27), [D(2026, 12, 26)]), 5)

    def test_reversed_range(self):
        self.assertEqual(business_days(D(2026, 9, 13), D(2026, 9, 7)), 0)
