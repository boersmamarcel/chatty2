import unittest

from gradebook.loader import LoaderError, load_assessments, load_marks, parse_points
from gradebook.models import EXCUSED

ASSESSMENTS = """key,category,max_points,due
hw1,homework,10,2026-02-01
hw2,homework,20,2026-02-08
"""


class ParsePointsTest(unittest.TestCase):
    def test_numbers(self):
        self.assertEqual(parse_points("8"), 8.0)
        self.assertEqual(parse_points(" 12 "), 12.0)
        self.assertEqual(parse_points("7,5"), 7.5)
        self.assertEqual(parse_points("7.25"), 7.25)
        self.assertEqual(parse_points("0"), 0.0)
        self.assertIsInstance(parse_points("7,5"), float)

    def test_missing(self):
        self.assertIsNone(parse_points(""))
        self.assertIsNone(parse_points("   "))

    def test_excused(self):
        for text in ["EX", "ex", " Ex ", "eX"]:
            self.assertIs(parse_points(text), EXCUSED, msg=text)

    def test_invalid(self):
        for bad in ["-1", "abc", "1,2,3", "1.5,5", "-0,5", "EXC"]:
            with self.assertRaises(LoaderError, msg=bad):
                parse_points(bad)


class LoadMarksTest(unittest.TestCase):
    def test_values_kept(self):
        text = ("student_id,assessment,points,submitted,extension_days\n"
                "s1,hw1,,,\n"
                "s1,hw2,EX,,\n"
                "s2,hw1,\"7,5\",2026-02-01,\n")
        marks = load_marks(text, load_assessments(ASSESSMENTS))
        self.assertIsNone(marks[0].points)
        self.assertIs(marks[1].points, EXCUSED)
        self.assertTrue(marks[1].excused)
        self.assertEqual(marks[2].points, 7.5)

    def test_line_numbers(self):
        text = ("student_id,assessment,points\n"
                "s1,hw1,8\n"
                "s1,hw2,abc\n")
        with self.assertRaises(LoaderError) as ctx:
            load_marks(text, load_assessments(ASSESSMENTS))
        self.assertTrue(str(ctx.exception).startswith("marks line 3: "), str(ctx.exception))
        text = ("student_id,assessment,points\n"
                "s1,quiz,8\n")
        with self.assertRaises(LoaderError) as ctx:
            load_marks(text, load_assessments(ASSESSMENTS))
        self.assertTrue(str(ctx.exception).startswith("marks line 2: "), str(ctx.exception))
        text = ("student_id,assessment,points,submitted\n"
                "s1,hw1,8,\n"
                "s1,hw2,9,\n"
                "s1,hw1,4,not-a-date\n")
        with self.assertRaises(LoaderError) as ctx:
            load_marks(text)
        self.assertTrue(str(ctx.exception).startswith("marks line 4: "), str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
