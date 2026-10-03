import datetime
import unittest
from decimal import Decimal

from ledgerly.accounts import default_chart
from ledgerly.invoicing import Invoice, build_entry
from ledgerly.ledger import Ledger
from ledgerly.tax import compute_tax, default_tax_table, split_gross

D = datetime.date
X = Decimal


class ComputeTaxTest(unittest.TestCase):
    def test_half_up(self):
        self.assertEqual(compute_tax(X("0.50"), X("0.09")), X("0.05"))
        self.assertEqual(compute_tax(X("2.50"), X("0.21")), X("0.53"))
        self.assertEqual(compute_tax(X("0.15"), X("0.10")), X("0.02"))
        self.assertEqual(compute_tax(X("1.05"), X("0.10")), X("0.11"))

    def test_jpy_has_no_decimals(self):
        result = compute_tax(X("1050"), X("0.21"), "JPY")
        self.assertEqual(result, X("221"))
        self.assertEqual(str(result), "221")

    def test_exponent_is_currency_minor_units(self):
        self.assertEqual(str(compute_tax(X("10"), X("0.10"))), "1.00")
        self.assertEqual(str(compute_tax(X("10.00"), X("0"))), "0.00")
        self.assertEqual(str(compute_tax(X("12.5"), X("0.2"), "BHD")), "2.500")
        self.assertIsInstance(compute_tax(X("3"), X("0.21")), Decimal)


class SplitGrossTest(unittest.TestCase):
    def test_example(self):
        self.assertEqual(split_gross(X("10.00"), X("0.21")), (X("8.26"), X("1.74")))

    def test_parts_always_add_up(self):
        for cents in range(1, 400, 7):
            gross = X(cents) / 100
            for rate in (X("0.21"), X("0.09"), X("0.06")):
                net, tax = split_gross(gross, rate)
                self.assertEqual(net + tax, gross, (gross, rate))

    def test_tax_is_rounded_half_up_on_gross(self):
        # 1.09 * 0.09 / 1.09 = 0.09 exactly; 0.5 * 0.21 / 1.21 = 0.0867.. -> 0.09
        self.assertEqual(split_gross(X("1.09"), X("0.09")), (X("1.00"), X("0.09")))
        self.assertEqual(split_gross(X("0.50"), X("0.21")), (X("0.41"), X("0.09")))
        self.assertEqual(split_gross(X("121"), X("0.21"), "JPY"), (X("100"), X("21")))


def invoice():
    inv = Invoice("INV-7", "Acme", D(2024, 3, 1), D(2024, 3, 31))
    inv.add_line("Stickers", 1, "0.50", "R")
    inv.add_line("Stickers", 1, "0.50", "R")
    inv.add_line("Stickers", 1, "0.50", "r")
    inv.add_line("Consulting", 2, "12.25", "S", account="4100")
    inv.add_line("Postage", 1, "0.25", "S", account="4100")
    inv.add_line("Training", 1, "10.00", "E", account="4100")
    return inv


class BreakdownTest(unittest.TestCase):
    def test_issue_example(self):
        inv = Invoice("INV-1", "Acme", D(2024, 3, 1), D(2024, 3, 31))
        for _ in range(3):
            inv.add_line("Item", 1, "0.50", "R")
        self.assertEqual(inv.tax_breakdown(default_tax_table()), [("R", X("1.50"), X("0.14"))])

    def test_rounded_once_per_code(self):
        table = default_tax_table()
        # S: 24.50 + 0.25 = 24.75 * 0.21 = 5.1975 -> 5.20 (per line: 5.15 + 0.05 = 5.20)
        self.assertEqual(invoice().tax_breakdown(table), [
            ("E", X("10.00"), X("0.00")),
            ("R", X("1.50"), X("0.14")),
            ("S", X("24.75"), X("5.20")),
        ])

    def test_per_code_differs_from_per_line(self):
        inv = Invoice("INV-2", "Beta", D(2024, 3, 1), D(2024, 3, 31))
        for _ in range(4):
            inv.add_line("Bolt", 1, "0.10", "S")
        # 0.40 * 0.21 = 0.084 -> 0.08 (per line 4 * 0.02 = 0.08); add one more 0.10 -> 0.50 * 0.21 = 0.105 -> 0.11
        inv.add_line("Bolt", 1, "0.10", "S")
        self.assertEqual(inv.tax_breakdown(default_tax_table()), [("S", X("0.50"), X("0.11"))])

    def test_totals(self):
        table = default_tax_table()
        inv = invoice()
        self.assertEqual(inv.tax_total(table), X("5.34"))
        self.assertEqual(inv.total(table), X("41.59"))

    def test_entry_lines_and_posting(self):
        table = default_tax_table()
        entry = build_entry(invoice(), table)
        got = [(l.account, l.side, l.amount, l.memo) for l in entry.lines]
        self.assertEqual(got, [
            ("1200", "D", X("41.59"), ""),
            ("4000", "C", X("1.50"), ""),
            ("4100", "C", X("34.75"), ""),
            ("2100", "C", X("0.14"), "VAT R"),
            ("2100", "C", X("5.20"), "VAT S"),
        ])
        ledger = Ledger(default_chart())
        ledger.post(entry)
        self.assertEqual(ledger.balance("2100"), X("5.34"))


if __name__ == "__main__":
    unittest.main()
