import unittest
from decimal import Decimal

from stockroom.money import format_amount
from stockroom.reports import stock_value_report


class FormatAmountTest(unittest.TestCase):
    def test_grouping(self):
        self.assertEqual(format_amount(Decimal("1234567.891")), "1,234,567.89")
        self.assertEqual(format_amount(Decimal("999.995")), "1,000.00")
        self.assertEqual(format_amount(Decimal("12.5")), "12.50")
        self.assertEqual(format_amount(Decimal("1000"), places=0), "1,000")

    def test_negative(self):
        self.assertEqual(format_amount(Decimal("-1234.5")), "-1,234.50")
        self.assertEqual(format_amount(Decimal("-1234.5"), accounting=True), "(1,234.50)")
        self.assertEqual(format_amount(Decimal("-0.125"), accounting=True), "(0.13)")
        self.assertEqual(format_amount(Decimal("1234.5"), accounting=True), "1,234.50")

    def test_zero(self):
        for value in (Decimal("0"), Decimal("-0.004"), Decimal("-0.00"), Decimal("0.001")):
            self.assertEqual(format_amount(value), "0.00")
            self.assertEqual(format_amount(value, accounting=True), "0.00")


class StockValueReportTest(unittest.TestCase):
    def test_example(self):
        rows = [("AB-1", 12, Decimal("2.50")), ("LONGSKU-77", -3, Decimal("1500.00")),
                ("C", 1000, Decimal("12.345"))]
        expected = "\n".join([
            "SKU          QTY       VALUE",
            "AB-1          12       30.00",
            "LONGSKU-77    -3  (4,500.00)",
            "C           1000   12,345.00",
            "TOTAL       1009    7,875.00",
        ])
        self.assertEqual(stock_value_report(rows), expected)

    def test_narrow(self):
        rows = [("A", 1, Decimal("1.00"))]
        expected = "\n".join([
            "SKU    QTY  VALUE",
            "A        1   1.00",
            "TOTAL    1   1.00",
        ])
        self.assertEqual(stock_value_report(rows), expected)

    def test_negative_total(self):
        rows = [("VERY-LONG-SKU-0001", -2000, Decimal("600.005")), ("X", 5, Decimal("0.10"))]
        report = stock_value_report(rows)
        lines = report.split("\n")
        self.assertEqual(lines[0], "SKU                   QTY           VALUE")
        self.assertEqual(lines[1], "VERY-LONG-SKU-0001  -2000  (1,200,010.00)")
        self.assertEqual(lines[2], "X                       5            0.50")
        self.assertEqual(lines[3], "TOTAL               -1995  (1,200,009.50)")
        self.assertFalse(report.endswith("\n"))
        for line in lines:
            self.assertEqual(line, line.rstrip())


if __name__ == "__main__":
    unittest.main()
