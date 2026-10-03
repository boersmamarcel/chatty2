import datetime
import unittest
from decimal import Decimal

from ledgerly.accounts import default_chart
from ledgerly.errors import ImportErrors
from ledgerly.importer import parse_amount, parse_journal_csv

X = Decimal

BAD = "\n".join([
    "entry,date,account,debit,credit,currency,description",
    "E1,2024-03-05,1100,100.00,,,ok",
    "E1,2024-03-05,4000,,100.00,,",
    "",
    "E2,2024-13-01,1100,10,,,bad date",
    "E2,2024-03-06,9999,,10,,",
    "E3,2024-03-07,1100,abc,,,",
    "E3,2024-03-07,4000,,5,,",
    'E4,2024-03-08,1100,"1,000.00",,,',
    'E4,2024-03-08,4000,,"1,000.00",,',
    "E5,2024-03-09,1100,5,5,,",
    "E6,2024-03-10,6000,,,,",
    ",2024-03-10,6000,1,,,",
    "E7,2024-03-11,6000,100.00,,,",
    "E7,2024-03-11,1100,,90.00,,",
    "",
    "",
    "E8,12.03.2024,1100,20,,,",
    "E8,12.03.2024,4000,,25.50,,",
    "E9,2024-03-13,1100,7,,usd,",
    "E9,2024-03-13,4000,,6.44,,",
    "",
])


class AmountTest(unittest.TestCase):
    def test_separators(self):
        self.assertEqual(parse_amount("1,234.50"), X("1234.50"))
        self.assertEqual(parse_amount(" 1,234,567.5 "), X("1234567.5"))
        self.assertEqual(parse_amount("999"), X("999"))
        self.assertEqual(parse_amount(""), X("0"))


class CollectErrorsTest(unittest.TestCase):
    def test_all_errors_in_order(self):
        with self.assertRaises(ImportErrors) as ctx:
            parse_journal_csv(BAD, default_chart())
        self.assertEqual(ctx.exception.errors, [
            "line 5: invalid date '2024-13-01'",
            "line 6: unknown account 9999",
            "line 7: invalid amount 'abc'",
            "line 11: both debit and credit given",
            "line 12: neither debit nor credit given",
            "line 13: missing entry reference",
            "entry E7: unbalanced (debit 100.00, credit 90.00)",
            "entry E8: unbalanced (debit 20.00, credit 25.50)",
        ])

    def test_only_unbalanced(self):
        text = "\n".join([
            "entry,date,account,debit,credit,currency,description",
            "A,2024-01-02,1100,10,,,",
            "A,2024-01-02,4000,,9,,",
            "B,2024-01-03,1100,5,,,",
            "B,2024-01-03,4000,,5,,",
            "C,2024-01-04,1100,,3,,",
            "C,2024-01-04,6000,1.5,,,",
        ])
        with self.assertRaises(ImportErrors) as ctx:
            parse_journal_csv(text, default_chart())
        self.assertEqual(ctx.exception.errors, [
            "entry A: unbalanced (debit 10.00, credit 9.00)",
            "entry C: unbalanced (debit 1.50, credit 3.00)",
        ])

    def test_line_numbers_count_blank_lines(self):
        text = "entry,date,account,debit,credit,currency,description\n\n\nA,2024-01-02,1100,10,,,\nA,2024-01-02,77777,,10,,\n"
        with self.assertRaises(ImportErrors) as ctx:
            parse_journal_csv(text, default_chart())
        self.assertEqual(ctx.exception.errors, ["line 5: unknown account 77777"])

    def test_first_data_row_is_line_two(self):
        text = "entry,date,account,debit,credit,currency,description\nA,2024-02-30,1100,10,,,\nA,2024-01-02,4000,,10,,\n"
        with self.assertRaises(ImportErrors) as ctx:
            parse_journal_csv(text, default_chart())
        self.assertEqual(ctx.exception.errors, ["line 2: invalid date '2024-02-30'"])


class GoodFileTest(unittest.TestCase):
    def test_entries_returned(self):
        text = "\n".join([
            "entry,date,account,debit,credit,currency,description",
            'E1,05.03.2024,1100," 2,500.00 ",,,Sale',
            "",
            'E1,05.03.2024,4000,,"2,500.00",EUR,',
            "E2,2024-03-06,1100,7,,usd,fx",
            "E2,2024-03-06,4000,,6.44,,",
        ])
        entries = parse_journal_csv(text, default_chart())
        self.assertEqual([e.reference for e in entries], ["E1", "E2"])
        self.assertEqual(entries[0].date, datetime.date(2024, 3, 5))
        self.assertEqual(entries[0].totals(), (X("2500.00"), X("2500.00")))
        self.assertEqual([l.currency for l in entries[0].lines], [None, None])
        self.assertEqual(entries[1].currencies(), ["USD"])


if __name__ == "__main__":
    unittest.main()
