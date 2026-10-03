import unittest

from splitbill.settle import describe, settle


class SettleTest(unittest.TestCase):
    def test_largest_first(self):
        balances = {"anna": 5000, "ben": 1000, "cleo": -3000, "dirk": -3000}
        self.assertEqual(settle(balances), [
            ("cleo", "anna", 3000),
            ("dirk", "anna", 2000),
            ("dirk", "ben", 1000),
        ])

    def test_describe(self):
        self.assertEqual(describe([("cleo", "anna", 3000)]), "cleo pays anna 30.00 EUR")


if __name__ == "__main__":
    unittest.main()
