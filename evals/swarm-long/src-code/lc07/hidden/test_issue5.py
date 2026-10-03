import unittest

from gradebook.report import class_stats, format_report, median, rank_rows, stdev


class StatsTest(unittest.TestCase):
    def test_median(self):
        self.assertEqual(median([1, 2, 3, 4]), 2.5)
        self.assertEqual(median([4, 1, 3, 2]), 2.5)
        self.assertEqual(median([5, 1, 3]), 3)
        self.assertEqual(median([7.0]), 7.0)
        self.assertEqual(median([70.0, 80.0]), 75.0)

    def test_stdev(self):
        self.assertAlmostEqual(stdev([2, 4, 4, 4, 5, 5, 7, 9]), 2.13808994, places=6)
        self.assertEqual(stdev([42.0]), 0.0)
        self.assertEqual(stdev([]), 0.0)
        self.assertAlmostEqual(stdev([60.0, 80.0]), 14.14213562, places=6)

    def test_class_stats(self):
        stats = class_stats([60.0, 70.0, 80.0, 90.0])
        self.assertEqual(stats["count"], 4)
        self.assertEqual(stats["median"], 75.0)
        self.assertEqual(stats["stdev"], 12.91)
        self.assertEqual(stats["mean"], 75.0)
        with self.assertRaises(ValueError):
            class_stats([])

    def test_ranks(self):
        rows = [("Dan", 70.0, "C-"), ("Bea", 81.5, "B-"), ("Ann", 81.5, "B-"), ("Cy", 90.0, "A-"),
                ("Eve", 70.0, "C-"), ("Fay", 50.0, "F")]
        self.assertEqual(rank_rows(rows), [
            (1, "Cy", 90.0, "A-"),
            (2, "Ann", 81.5, "B-"),
            (2, "Bea", 81.5, "B-"),
            (4, "Dan", 70.0, "C-"),
            (4, "Eve", 70.0, "C-"),
            (6, "Fay", 50.0, "F"),
        ])
        self.assertEqual(rank_rows([]), [])

    def test_report_uses_ranks(self):
        text = format_report([("Bo", 80.0, "B-"), ("Al", 80.0, "B-")])
        self.assertIn("   1  Al", text)
        self.assertIn("   1  Bo", text)


if __name__ == "__main__":
    unittest.main()
