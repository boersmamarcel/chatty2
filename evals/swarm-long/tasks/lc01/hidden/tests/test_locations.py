import unittest

from stockroom.locations import LocationError, parse_bin, sort_bins


class LocationsTest(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(tuple(parse_bin("B-7-2")), ("B", 7, 2))
        self.assertEqual(str(parse_bin("B-7-2")), "B-07-2")

    def test_rack_zero_rejected(self):
        with self.assertRaises(LocationError):
            parse_bin("A-0-1")

    def test_sort_is_numeric(self):
        self.assertEqual(sort_bins(["A-100-1", "A-99-1", "A-9-2", "A-9-1"]),
                         ["A-09-1", "A-09-2", "A-99-1", "A-100-1"])

    def test_double_letter_aisles_after_z(self):
        self.assertEqual(sort_bins(["AA-1-1", "B-1-1", "Z-3-1"]),
                         ["B-01-1", "Z-03-1", "AA-01-1"])


if __name__ == "__main__":
    unittest.main()
