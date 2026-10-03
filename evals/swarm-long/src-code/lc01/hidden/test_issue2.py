import unittest
from decimal import Decimal

from stockroom.catalog import CatalogError, Item, load_catalog, parse_pack
from stockroom.units import UnitError, to_each

CATALOG = """sku,name,category,pack,case_boxes,unit_cost,reorder_point,min_order_qty,order_multiple,lead_time_days,status
ab-1,Gloves,ppe,12x6,4,0.40,100,0,1,3,active
CD-2,Tape,packing,24,10,1.10,50,0,1,2,active
"""


class ParsePackTest(unittest.TestCase):
    def test_single(self):
        self.assertEqual(parse_pack("24"), 24)
        self.assertEqual(parse_pack(" 24 "), 24)

    def test_multi_level(self):
        self.assertEqual(parse_pack("12x6"), 72)
        self.assertEqual(parse_pack("2x3x4"), 24)
        self.assertEqual(parse_pack("12X6"), 72)
        self.assertEqual(parse_pack("12 x 6"), 72)
        self.assertEqual(parse_pack(" 12 X 6 "), 72)

    def test_invalid(self):
        for bad in ["", "   ", "x6", "12x", "0", "12x0", "-3", "abc", "1.5", "12xx6"]:
            with self.assertRaises(CatalogError, msg=bad):
                parse_pack(bad)
        with self.assertRaises(CatalogError):
            parse_pack(None)

    def test_load_catalog(self):
        items = load_catalog(CATALOG)
        self.assertEqual(items["AB-1"].pack_size, 72)
        self.assertEqual(items["CD-2"].pack_size, 24)


class ToEachTest(unittest.TestCase):
    def setUp(self):
        self.item = Item("P4", pack_size=4, case_boxes=3)

    def test_whole(self):
        self.assertEqual(to_each(Decimal("1.5"), "BX", self.item), 6)
        self.assertEqual(to_each(2, "box", self.item), 8)
        self.assertEqual(to_each(-2, "BX", self.item), -8)
        self.assertEqual(to_each(Decimal("0.25"), "CS", self.item), 3)
        self.assertIsInstance(to_each(Decimal("1.5"), "BX", self.item), int)
        self.assertEqual(to_each("7", "EA", self.item), 7)

    def test_fraction_rejected(self):
        with self.assertRaises(UnitError):
            to_each(Decimal("1.3"), "BX", self.item)
        with self.assertRaises(UnitError):
            to_each("0.5", "EA", self.item)
        with self.assertRaises(UnitError):
            to_each(Decimal("-1.1"), "BX", self.item)


if __name__ == "__main__":
    unittest.main()
