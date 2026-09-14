import unittest
from bank import Account

class TestBank(unittest.TestCase):
    def test_deposit(self):
        a = Account(10)
        self.assertEqual(a.deposit(5), 15)

if __name__ == "__main__":
    unittest.main()
