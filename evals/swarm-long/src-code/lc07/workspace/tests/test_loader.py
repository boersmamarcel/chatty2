import unittest

from gradebook.grading import course_percent
from gradebook.loader import LoaderError, load_assessments, load_marks, load_roster
from gradebook.policy import PolicyError, parse_policy

ROSTER = """student_id,name,section
s1,Ada Lovelace,A
s2, Alan Turing ,B
"""

ASSESSMENTS = """key,category,max_points,due
hw1,Homework,10,2026-02-01
hw2,homework,20,2026-02-08
mid,exam,50,
"""

MARKS = """student_id,assessment,points,submitted,extension_days
s1,hw1,8,2026-02-01 10:00,
s1,hw2,15,2026-02-08,
s1,mid,40,,
"""

POLICY = """# weights
homework 40
exam     60
"""


class LoaderTest(unittest.TestCase):
    def test_roster(self):
        roster = load_roster(ROSTER)
        self.assertEqual(list(roster), ["s1", "s2"])
        self.assertEqual(roster["s2"].name, "Alan Turing")

    def test_assessments(self):
        assessments = load_assessments(ASSESSMENTS)
        self.assertEqual(assessments["hw1"].category, "homework")
        self.assertEqual(assessments["hw2"].max_points, 20.0)
        self.assertIsNone(assessments["mid"].due)

    def test_marks_and_course_percent(self):
        assessments = load_assessments(ASSESSMENTS)
        marks = load_marks(MARKS, assessments)
        self.assertEqual([m.points for m in marks], [8.0, 15.0, 40.0])
        policies = parse_policy(POLICY)
        # homework 23/30, exam 40/50
        self.assertAlmostEqual(course_percent("s1", policies, assessments, marks),
                               0.4 * 100 * 23 / 30 + 0.6 * 80.0)

    def test_unknown_assessment(self):
        with self.assertRaises(LoaderError):
            load_marks("student_id,assessment,points\ns1,quiz9,3\n", load_assessments(ASSESSMENTS))

    def test_policy_weights(self):
        with self.assertRaises(PolicyError):
            parse_policy("homework 40\nexam 50\n")


if __name__ == "__main__":
    unittest.main()
