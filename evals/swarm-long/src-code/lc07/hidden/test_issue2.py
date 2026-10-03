import datetime
import unittest

from gradebook.models import Assessment
from gradebook.policy import PolicyError, drop_lowest


def day(n):
    return datetime.datetime(2026, 2, n, 23, 59)


Q1 = Assessment("q1", "quizzes", 5, day(1))
Q2 = Assessment("q2", "quizzes", 20, day(2))
Q3 = Assessment("q3", "quizzes", 10, day(3))
Q4 = Assessment("q4", "quizzes", 10, day(4))
QX = Assessment("qx", "quizzes", 10, None)
QA = Assessment("qa", "quizzes", 10, None)


def keys(entries):
    return [a.key for a, _ in entries]


class DropLowestTest(unittest.TestCase):
    def test_by_percentage(self):
        entries = [(Q1, 4.0), (Q2, 9.0), (Q3, 7.0)]
        self.assertEqual(keys(drop_lowest(entries, 1)), ["q1", "q3"])
        self.assertEqual(keys(drop_lowest(entries, 2)), ["q1"])

    def test_original_order_and_copy(self):
        entries = [(Q3, 9.0), (Q1, 1.0), (Q2, 18.0)]
        kept = drop_lowest(entries, 1)
        self.assertEqual(keys(kept), ["q3", "q2"])
        self.assertEqual(kept, [(Q3, 9.0), (Q2, 18.0)])
        self.assertEqual(len(entries), 3)
        same = drop_lowest(entries, 0)
        self.assertEqual(same, entries)
        self.assertIsNot(same, entries)

    def test_tie_earlier_due_dropped_first(self):
        entries = [(Q4, 5.0), (Q3, 5.0), (Q2, 18.0)]
        self.assertEqual(keys(drop_lowest(entries, 1)), ["q4", "q2"])

    def test_tie_undated_after_dated_then_key(self):
        entries = [(QX, 5.0), (Q4, 5.0), (QA, 5.0), (Q2, 19.0)]
        self.assertEqual(keys(drop_lowest(entries, 1)), ["qx", "qa", "q2"])
        self.assertEqual(keys(drop_lowest(entries, 2)), ["qx", "q2"])

    def test_keep_at_least_one(self):
        entries = [(Q1, 4.0), (Q2, 9.0)]
        self.assertEqual(keys(drop_lowest(entries, 2)), ["q1"])
        self.assertEqual(keys(drop_lowest(entries, 5)), ["q1"])
        self.assertEqual(drop_lowest([], 1), [])
        self.assertEqual(keys(drop_lowest([(Q3, 0.0)], 1)), ["q3"])

    def test_keep_one_tie(self):
        entries = [(Q3, 5.0), (Q4, 5.0)]
        self.assertEqual(keys(drop_lowest(entries, 3)), ["q4"])

    def test_negative(self):
        with self.assertRaises(PolicyError):
            drop_lowest([(Q1, 1.0)], -1)


if __name__ == "__main__":
    unittest.main()
