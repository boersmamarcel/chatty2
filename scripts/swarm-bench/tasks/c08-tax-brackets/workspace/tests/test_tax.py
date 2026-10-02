import unittest

from tax import tax


class TaxTest(unittest.TestCase):
    def test_zero_bracket(self):
        self.assertEqual(tax(8000), 0.0)

    def test_second_bracket(self):
        self.assertEqual(tax(20000), 2000.0)

    def test_top_bracket(self):
        self.assertEqual(tax(50000), 10000.0)

    def test_boundaries(self):
        self.assertEqual(tax(10000), 0.0)
        self.assertEqual(tax(40000), 6000.0)

    def test_negative(self):
        self.assertEqual(tax(-5), 0.0)
