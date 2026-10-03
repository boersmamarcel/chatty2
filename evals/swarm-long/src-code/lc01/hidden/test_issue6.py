import datetime
import unittest

from stockroom.io_csv import read_movements

D = datetime.date
HEADER = "movement_id,date,sku,kind,qty,unit,lot,expiry,bin,note\n"


class Issue6Test(unittest.TestCase):
    def test_thousands_separator(self):
        text = HEADER + 'M1,2026-03-04,AB-1,RECEIPT,"1,200",EA,L1,,,\n' \
                        'M2,2026-03-05,AB-1,PICK,"1,050",,L1,,,\n'
        result = read_movements(text)
        self.assertEqual(result.errors, [])
        self.assertEqual([m.qty for m in result.movements], [1200, -1050])

    def test_day_month_year_dates(self):
        text = HEADER + "M1,04/03/2026,AB-1,RECEIPT,5,EA,L1,,,\n" \
                        "M2,2026-03-05,AB-1,RECEIPT,5,EA,L1,,,\n" \
                        "M3,31/12/2026,AB-1,RECEIPT,5,EA,L1,,,\n"
        result = read_movements(text)
        self.assertEqual(result.errors, [])
        self.assertEqual([m.date for m in result.movements],
                         [D(2026, 3, 4), D(2026, 3, 5), D(2026, 12, 31)])

    def test_bad_dates_are_row_errors(self):
        text = HEADER + "M1,2026/03/04,AB-1,RECEIPT,5,EA,L1,,,\n" \
                        "M2,13/13/2026,AB-1,RECEIPT,5,EA,L1,,,\n" \
                        "M3,03-04-2026,AB-1,RECEIPT,5,EA,L1,,,\n" \
                        "M4,2026-03-04,AB-1,RECEIPT,5,EA,L1,,,\n"
        result = read_movements(text)
        self.assertEqual([m.movement_id for m in result.movements], ["M4"])
        self.assertEqual([e.split(":")[0] for e in result.errors], ["line 2", "line 3", "line 4"])

    def test_duplicates_skipped(self):
        text = HEADER + "M1,2026-03-04,AB-1,RECEIPT,10,EA,L1,,,\n" \
                        "M2,2026-03-04,AB-1,RECEIPT,20,EA,L1,,,\n" \
                        "M1,2026-03-04,AB-1,RECEIPT,10,EA,L1,,,\n" \
                        "M1,2026-03-06,AB-1,RECEIPT,99,EA,L1,,,\n" \
                        "M2,2026-03-04,AB-1,RECEIPT,20,EA,L1,,,\n"
        result = read_movements(text)
        self.assertEqual([(m.movement_id, m.qty) for m in result.movements], [("M1", 10), ("M2", 20)])
        self.assertEqual(result.duplicates, ["M1", "M1", "M2"])
        self.assertEqual(result.errors, [])

    def test_id_of_rejected_row_is_not_a_duplicate(self):
        text = HEADER + "M1,2026-03-04,AB-1,RECEIPT,abc,EA,L1,,,\n" \
                        "M1,2026-03-04,AB-1,RECEIPT,10,EA,L1,,,\n"
        result = read_movements(text)
        self.assertEqual([(m.movement_id, m.qty) for m in result.movements], [("M1", 10)])
        self.assertEqual(result.duplicates, [])
        self.assertEqual(len(result.errors), 1)
        self.assertTrue(result.errors[0].startswith("line 2: "), result.errors)

    def test_line_numbers_count_header_and_blank_lines(self):
        text = HEADER + "M1,2026-03-04,AB-1,RECEIPT,10,EA,L1,,,\n" \
                        "\n" \
                        "M2,2026-03-04,AB-1,BOGUS,10,EA,L1,,,\n" \
                        "\n" \
                        "\n" \
                        "M3,2026-03-04,,RECEIPT,10,EA,L1,,,\n" \
                        "M4,2026-03-04,AB-1,RECEIPT,7,EA,L1,,,\n"
        result = read_movements(text)
        self.assertEqual([m.movement_id for m in result.movements], ["M1", "M4"])
        self.assertEqual([e.split(":")[0] for e in result.errors], ["line 4", "line 7"])


if __name__ == "__main__":
    unittest.main()
