import unittest

from gradebook.grading import letter_for


class LetterTest(unittest.TestCase):
    def test_inside_bands(self):
        self.assertEqual(letter_for(95.0), "A")
        self.assertEqual(letter_for(85.5), "B")
        self.assertEqual(letter_for(42.0), "F")

    def test_boundary_is_inclusive(self):
        self.assertEqual(letter_for(90.0), "A-")
        self.assertEqual(letter_for(60.0), "D")

    def test_rounded_to_one_decimal(self):
        self.assertEqual(letter_for(89.96), "A-")


if __name__ == "__main__":
    unittest.main()
