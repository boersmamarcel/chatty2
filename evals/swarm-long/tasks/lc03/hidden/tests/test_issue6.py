import unittest
from datetime import date, datetime
from decimal import Decimal

from rota.intervals import Interval
from rota.models import Employee, Shift
from rota.report import weekly_hours


def dt(day, hour, minute=0):
    return datetime(2026, 3, day, hour, minute)


class ClipTest(unittest.TestCase):

    def test_clip_end(self):
        iv = Interval(dt(8, 22), dt(9, 6))
        self.assertEqual(iv.clip(dt(2, 0), dt(9, 0)), Interval(dt(8, 22), dt(9, 0)))

    def test_clip_start(self):
        iv = Interval(dt(8, 22), dt(9, 6))
        self.assertEqual(iv.clip(dt(9, 0), dt(16, 0)), Interval(dt(9, 0), dt(9, 6)))

    def test_clip_both(self):
        iv = Interval(dt(1, 0), dt(20, 0))
        self.assertEqual(iv.clip(dt(2, 0), dt(9, 0)), Interval(dt(2, 0), dt(9, 0)))

    def test_clip_inside_and_touching(self):
        iv = Interval(dt(3, 8), dt(3, 16))
        self.assertEqual(iv.clip(dt(2, 0), dt(9, 0)), iv)
        self.assertIsNone(iv.clip(dt(3, 16), dt(4, 0)))
        self.assertIsNone(iv.clip(dt(3, 0), dt(3, 8)))


EMPLOYEES = [Employee(e, e.lower()) for e in ("E1", "E2", "E3", "E4", "E5", "E6")]


def shifts():
    return [
        Shift("A", dt(2, 6), dt(2, 8, 15), "nurse", employee="E1"),     # 135 min
        Shift("B", dt(3, 9), dt(3, 11, 3), "nurse", employee="E2"),     # 123 min
        Shift("C", dt(1, 22), dt(2, 6), "nurse", employee="E4"),        # 6 h in this week
        Shift("D", dt(8, 22), dt(9, 6), "nurse", employee="E4"),        # 2 h in this week
        Shift("E", dt(4, 8), dt(4, 9), "nurse", employee="E5"),         # 60 min
        Shift("F", dt(5, 8), dt(5, 9, 15), "nurse", employee="E5"),     # 75 min
        Shift("G", dt(6, 8), dt(6, 10, 17), "nurse", employee="E6"),    # 137 min
        Shift("H", dt(4, 8), dt(4, 20), "nurse"),                       # unassigned
        Shift("I", dt(4, 8), dt(4, 20), "nurse", employee="E99"),       # unknown employee
    ]


class WeeklyHoursTest(unittest.TestCase):

    def test_rows(self):
        rows = weekly_hours(shifts(), EMPLOYEES, date(2026, 3, 2))
        self.assertEqual([(emp, str(hours)) for emp, hours in rows], [
            ("E4", "8.0"), ("E1", "2.3"), ("E5", "2.3"), ("E6", "2.3"),
            ("E2", "2.1"), ("E3", "0.0"),
        ])

    def test_decimal_values(self):
        rows = weekly_hours(shifts(), EMPLOYEES, date(2026, 3, 2))
        for _, hours in rows:
            self.assertIsInstance(hours, Decimal)

    def test_next_week_gets_the_rest(self):
        rows = weekly_hours(shifts(), EMPLOYEES[3:4], date(2026, 3, 9))
        self.assertEqual([(emp, str(hours)) for emp, hours in rows], [("E4", "6.0")])

    def test_only_listed_employees(self):
        rows = weekly_hours(shifts(), [Employee("E2", "b"), Employee("E1", "a")], date(2026, 3, 2))
        self.assertEqual([(emp, str(hours)) for emp, hours in rows], [("E1", "2.3"), ("E2", "2.1")])

    def test_empty(self):
        rows = weekly_hours([], [Employee("E2", "b"), Employee("E1", "a")], date(2026, 3, 2))
        self.assertEqual([(emp, str(hours)) for emp, hours in rows], [("E1", "0.0"), ("E2", "0.0")])


if __name__ == "__main__":
    unittest.main()
