import unittest
from datetime import date

from rota.calendar import WorkingCalendar
from rota.holidays import HolidayRule, parse_rules

RULES = "Christmas Day = fixed 12-25 observed\nNew Year's Day = fixed 01-01 observed\n"


class ObservedRuleTest(unittest.TestCase):

    def test_saturday_moves_to_friday(self):
        rule = HolidayRule("Christmas Day", "fixed", month=12, day=25, observed=True)
        self.assertEqual(rule.observed_date(2021), date(2021, 12, 24))

    def test_sunday_moves_to_monday(self):
        rule = HolidayRule("Christmas Day", "fixed", month=12, day=25, observed=True)
        self.assertEqual(rule.observed_date(2022), date(2022, 12, 26))

    def test_weekday_not_moved(self):
        rule = HolidayRule("Christmas Day", "fixed", month=12, day=25, observed=True)
        self.assertEqual(rule.observed_date(2023), date(2023, 12, 25))

    def test_not_observed_rule_never_moves(self):
        rule = HolidayRule("Boxing Day", "fixed", month=12, day=26)
        self.assertEqual(rule.observed_date(2021), date(2021, 12, 26))
        self.assertEqual(rule.observed_date(2020), date(2020, 12, 26))

    def test_observed_may_leave_the_year(self):
        rule = HolidayRule("New Year's Day", "fixed", month=1, day=1, observed=True)
        self.assertEqual(rule.observed_date(2022), date(2021, 12, 31))


class CalendarHolidayTest(unittest.TestCase):

    def setUp(self):
        self.cal = WorkingCalendar(rules=parse_rules(RULES))

    def test_observed_and_actual_names(self):
        self.assertEqual(self.cal.holiday_name(date(2021, 12, 24)), "Christmas Day (observed)")
        self.assertEqual(self.cal.holiday_name(date(2021, 12, 25)), "Christmas Day")
        self.assertEqual(self.cal.holiday_name(date(2022, 12, 26)), "Christmas Day (observed)")
        self.assertEqual(self.cal.holiday_name(date(2022, 12, 25)), "Christmas Day")

    def test_weekday_holiday_listed_once(self):
        self.assertEqual(self.cal.holiday_name(date(2025, 12, 25)), "Christmas Day")
        self.assertIsNone(self.cal.holiday_name(date(2025, 12, 26)))
        self.assertIsNone(self.cal.holiday_name(date(2025, 12, 24)))

    def test_observed_day_not_working(self):
        self.assertFalse(self.cal.is_working_day(date(2021, 12, 24)))
        self.assertTrue(self.cal.is_working_day(date(2021, 12, 23)))
        self.assertFalse(self.cal.is_working_day(date(2022, 12, 26)))

    def test_previous_year_observance(self):
        self.assertEqual(self.cal.holiday_name(date(2021, 12, 31)), "New Year's Day (observed)")
        self.assertFalse(self.cal.is_working_day(date(2021, 12, 31)))
        self.assertEqual(self.cal.holiday_name(date(2022, 1, 1)), "New Year's Day")

    def test_add_working_days(self):
        self.assertEqual(self.cal.add_working_days(date(2021, 12, 23), 1), date(2021, 12, 27))
        self.assertEqual(self.cal.add_working_days(date(2021, 12, 30), 1), date(2022, 1, 3))
        self.assertEqual(self.cal.add_working_days(date(2022, 1, 3), -1), date(2021, 12, 30))

    def test_holidays_between(self):
        self.assertEqual(self.cal.holidays_between(date(2021, 12, 20), date(2022, 1, 5)), [
            (date(2021, 12, 24), "Christmas Day (observed)"),
            (date(2021, 12, 25), "Christmas Day"),
            (date(2021, 12, 31), "New Year's Day (observed)"),
            (date(2022, 1, 1), "New Year's Day"),
        ])

    def test_working_days_between(self):
        # Dec 20-31 2021: 10 weekdays minus observed Christmas (24th) and New Year (31st)
        self.assertEqual(self.cal.working_days_between(date(2021, 12, 20), date(2022, 1, 1)), 8)


if __name__ == "__main__":
    unittest.main()
