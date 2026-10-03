import unittest
from datetime import datetime

from rota.assign import assign_rota
from rota.intervals import Interval
from rota.models import Employee, Shift


def dt(day, hour, minute=0):
    return datetime(2026, 3, day, hour, minute)


def nurse(emp_id, name, time_off=()):
    return Employee(emp_id, name, skills={"nurse"}, time_off=time_off)


class IsAvailableTest(unittest.TestCase):

    def setUp(self):
        self.emp = nurse("E1", "Zoe", [Interval(dt(2, 0), dt(2, 6))])

    def test_touching_after_time_off(self):
        self.assertTrue(self.emp.is_available(dt(2, 6), dt(2, 14)))

    def test_touching_before_time_off(self):
        self.assertTrue(self.emp.is_available(dt(1, 16), dt(2, 0)))

    def test_overlapping(self):
        self.assertFalse(self.emp.is_available(dt(2, 5, 59), dt(2, 14)))
        self.assertFalse(self.emp.is_available(dt(1, 23), dt(2, 0, 1)))
        self.assertFalse(self.emp.is_available(dt(2, 1), dt(2, 2)))

    def test_far_away(self):
        self.assertTrue(self.emp.is_available(dt(3, 6), dt(3, 14)))


class AssignTest(unittest.TestCase):

    def test_time_off_respected(self):
        employees = [nurse("E1", "Zoe", [Interval(dt(2, 0), dt(2, 23))]), nurse("E2", "Adam")]
        result = assign_rota([Shift("S1", dt(2, 6), dt(2, 14), "nurse")], employees)
        self.assertEqual(result.assignments, {"S1": "E2"})

    def test_time_off_blocks_everyone(self):
        employees = [nurse("E1", "Zoe", [Interval(dt(2, 0), dt(2, 23))])]
        result = assign_rota([Shift("S1", dt(2, 6), dt(2, 14), "nurse")], employees)
        self.assertEqual(result.assignments, {})
        self.assertEqual(result.unfilled, ["S1"])

    def test_touching_time_off_still_assignable(self):
        employees = [nurse("E1", "Zoe", [Interval(dt(2, 0), dt(2, 6))]), nurse("E2", "Adam")]
        result = assign_rota([Shift("S1", dt(2, 6), dt(2, 14), "nurse")], employees)
        self.assertEqual(result.assignments, {"S1": "E1"})

    def test_tie_break_by_minutes_then_id(self):
        employees = [nurse("E1", "Zed"), nurse("E2", "Amy")]
        shifts = [Shift("S1", dt(2, 8), dt(2, 18), "nurse"),
                  Shift("S2", dt(3, 8), dt(3, 10), "nurse"),
                  Shift("S3", dt(3, 12), dt(3, 14), "nurse"),
                  Shift("S4", dt(4, 8), dt(4, 9), "nurse")]
        result = assign_rota(shifts, employees, min_rest_hours=0)
        self.assertEqual(result.assignments, {"S1": "E1", "S2": "E2", "S3": "E2", "S4": "E2"})
        self.assertEqual(result.minutes, {"E1": 600, "E2": 300})

    def test_id_compared_as_string(self):
        employees = [nurse("E9", "Anna"), nurse("E10", "Bert")]
        result = assign_rota([Shift("S1", dt(2, 6), dt(2, 14), "nurse")], employees)
        self.assertEqual(result.assignments, {"S1": "E10"})


if __name__ == "__main__":
    unittest.main()
