import unittest

from stockroom.locations import BinCode, LocationError, parse_bin, pick_path, sort_bins


class Issue5Test(unittest.TestCase):
    def test_parse_normalises(self):
        self.assertEqual(parse_bin(" b-7-2 "), BinCode("B", 7, 2))
        self.assertEqual(parse_bin("aa-012-0"), BinCode("AA", 12, 0))
        self.assertEqual(str(parse_bin("c-5-1\n")), "C-05-1")

    def test_parse_rejects(self):
        for bad in ["", "A-0-1", "a-00-1", "ABC-1-1", "A1-2-3", "A-1", "A-1-12", "A-1000-1", "A_1_1"]:
            with self.assertRaises(LocationError, msg=bad):
                parse_bin(bad)

    def test_sort_numeric_and_aisle_number(self):
        codes = ["A-100-1", "aa-1-1", "A-99-1", "B-2-1", "A-9-2", "Z-1-1", "A-9-1", "AB-1-1"]
        self.assertEqual(sort_bins(codes), ["A-09-1", "A-09-2", "A-99-1", "A-100-1", "B-02-1",
                                            "Z-01-1", "AA-01-1", "AB-01-1"])

    def test_sort_distinct(self):
        self.assertEqual(sort_bins(["B-7-2", " b-07-2", "B-007-2", "A-1-1"]), ["A-01-1", "B-07-2"])

    def test_pick_path_serpentine(self):
        codes = ["A-1-1", "A-12-1", "B-3-2", "B-3-1", "B-10-1", "C-2-1", "C-1-1",
                 "Z-5-1", "Z-6-1", "AA-4-1", "AA-2-1", "AB-1-1", "AB-9-1"]
        self.assertEqual(pick_path(codes), [
            "A-01-1", "A-12-1",
            "B-10-1", "B-03-1", "B-03-2",
            "C-01-1", "C-02-1",
            "Z-06-1", "Z-05-1",
            "AA-02-1", "AA-04-1",
            "AB-09-1", "AB-01-1",
        ])

    def test_pick_path_distinct_and_levels_ascending(self):
        self.assertEqual(pick_path(["d-4-3", "D-4-1", "D-04-3", "D-9-0"]),
                         ["D-09-0", "D-04-1", "D-04-3"])

    def test_empty(self):
        self.assertEqual(sort_bins([]), [])
        self.assertEqual(pick_path([]), [])


if __name__ == "__main__":
    unittest.main()
