import unittest

from splitbill.balances import BalanceError, net_balances
from splitbill.expenses import Expense
from splitbill.importer import ImportFailed, load_expenses

HEADER = "date,payer,amount,currency,description,split\n"


class RefundTest(unittest.TestCase):
    def test_import_refund(self):
        text = HEADER + "2026-03-04,anna,-30.00,,deposit refund,\"equal:anna,ben,cleo\"\n"
        expenses = load_expenses(text)
        self.assertEqual(expenses[0].amount, -3000)
        self.assertEqual(dict(expenses[0].shares), {"anna": -1000, "ben": -1000, "cleo": -1000})

    def test_import_refund_exact(self):
        text = HEADER + "2026-03-05,ben,-12.50,,returned shoes,\"exact:ben=-10.00,cleo=-2.50\"\n"
        expenses = load_expenses(text)
        self.assertEqual(expenses[0].amount, -1250)
        self.assertEqual(sum(expenses[0].shares.values()), -1250)

    def test_balances_with_refund(self):
        text = (HEADER
                + "2026-03-01,anna,90.00,,deposit,\"equal:anna,ben,cleo\"\n"
                + "2026-03-04,anna,-30.00,,deposit refund,\"equal:anna,ben,cleo\"\n")
        balances = net_balances(load_expenses(text))
        self.assertEqual(dict(balances), {"anna": 4000, "ben": -2000, "cleo": -2000})
        refund_only = net_balances(load_expenses(
            HEADER + "2026-03-04,anna,-30.00,,deposit refund,\"equal:anna,ben,cleo\"\n"))
        self.assertEqual(dict(refund_only), {"anna": -2000, "ben": 1000, "cleo": 1000})

    def test_zero_amount_still_rejected(self):
        with self.assertRaises(ImportFailed) as ctx:
            load_expenses(HEADER + "2026-03-01,anna,10.00,,a,\"equal:anna,ben\"\n"
                          + "2026-03-02,anna,0.00,,b,\"equal:anna,ben\"\n")
        self.assertTrue(str(ctx.exception).startswith("line 3"), str(ctx.exception))
        with self.assertRaises(BalanceError):
            net_balances([Expense("d", "anna", 0, "nothing", {"anna": 0})])

    def test_bad_shares_still_rejected(self):
        with self.assertRaises(BalanceError):
            net_balances([Expense("d", "anna", -300, "refund", {"anna": -100, "ben": -100})])
        balances = net_balances([Expense("d", "anna", -300, "refund", {"anna": -100, "ben": -200})])
        self.assertEqual(dict(balances), {"anna": -200, "ben": 200})


if __name__ == "__main__":
    unittest.main()
