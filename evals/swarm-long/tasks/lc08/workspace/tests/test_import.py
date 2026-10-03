import unittest

from splitbill.balances import net_balances
from splitbill.expenses import SplitError, parse_split
from splitbill.importer import ImportFailed, load_expenses

EXPENSES = """date,payer,amount,currency,description,split
2026-03-01,Anna,90.00,,groceries,"equal:anna,ben,cleo"
2026-03-02,ben,25.00,EUR,taxi,"exact:anna=10.00,ben=15.00"
2026-03-03,cleo,40.00,,museum,"shares:anna=1,cleo=3"
"""


class ImportTest(unittest.TestCase):
    def test_load(self):
        expenses = load_expenses(EXPENSES)
        self.assertEqual([e.amount for e in expenses], [9000, 2500, 4000])
        self.assertEqual(expenses[0].payer, "anna")
        self.assertEqual(dict(expenses[2].shares), {"anna": 1000, "cleo": 3000})

    def test_balances(self):
        balances = net_balances(load_expenses(EXPENSES))
        self.assertEqual(dict(balances), {"anna": 4000, "ben": -2000, "cleo": -2000})
        self.assertEqual(sum(balances.values()), 0)

    def test_errors(self):
        with self.assertRaises(ImportFailed):
            load_expenses("date,payer,amount\n")
        with self.assertRaises(SplitError):
            parse_split("exact:anna=1.00,ben=2.00", 400)
        with self.assertRaises(SplitError):
            parse_split("thirds:anna,ben", 300)


if __name__ == "__main__":
    unittest.main()
