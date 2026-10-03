import unittest

from gradebook.grading import GradingError, letter_for


class LetterBoundaryTest(unittest.TestCase):
    def test_examples(self):
        cases = [(90.0, "A-"), (89.95, "A-"), (89.94, "B+"), (93.0, "A"), (92.96, "A"),
                 (92.94, "A-"), (87.0, "B+"), (83.0, "B"), (80.0, "B-"), (77.0, "C+"),
                 (73.0, "C"), (70.0, "C-"), (69.95, "C-"), (60.0, "D"), (59.95, "D"),
                 (59.94, "F"), (0, "F"), (0.0, "F"), (100.0, "A")]
        for pct, letter in cases:
            self.assertEqual(letter_for(pct), letter, msg=pct)

    def test_extra_credit(self):
        self.assertEqual(letter_for(104.5), "A")

    def test_negative(self):
        with self.assertRaises(GradingError):
            letter_for(-0.5)
        with self.assertRaises(ValueError):
            letter_for(-10)


if __name__ == "__main__":
    unittest.main()
