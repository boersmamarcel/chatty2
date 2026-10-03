import datetime
import unittest
from decimal import Decimal

from ledgerly.accounts import default_chart
from ledgerly.importer import parse_journal_csv

GOOD = """entry,date,account,debit,credit,currency,description
E1,2024-03-05,1100,250.00,,,Cash sale
E1,2024-03-05,4000,,250.00,,
E2,06.03.2024,6000,80,,,Rent
E2,06.03.2024,1100,,80,,
"""

COMMAS = """entry,date,account,debit,credit,currency,description
E1,2024-03-05,1100,"1,234.50",,,Big sale
E1,2024-03-05,4000,,"1,234.50",,
E2,2024-03-06,6000, 12.00 ,,,Rent
E2,2024-03-06,1100,,12.00,,
"""


class ImporterTest(unittest.TestCase):
    def test_groups_rows_into_entries(self):
        entries = parse_journal_csv(GOOD, default_chart())
        self.assertEqual([e.reference for e in entries], ["E1", "E2"])
        self.assertEqual(entries[1].date, datetime.date(2024, 3, 6))
        self.assertEqual(entries[0].description, "Cash sale")
        self.assertEqual(entries[1].totals(), (Decimal("80"), Decimal("80")))

    def test_thousands_separators(self):
        entries = parse_journal_csv(COMMAS, default_chart())
        self.assertEqual(entries[0].totals(), (Decimal("1234.50"), Decimal("1234.50")))
        self.assertEqual(entries[1].lines[0].amount, Decimal("12.00"))


if __name__ == "__main__":
    unittest.main()
