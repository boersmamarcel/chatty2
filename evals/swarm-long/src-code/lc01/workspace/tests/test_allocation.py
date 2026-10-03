import datetime
import unittest

from stockroom.allocation import AllocationError, allocate
from stockroom.lots import Lot

D = datetime.date


class AllocationTest(unittest.TestCase):
    def test_earliest_expiry_first(self):
        lots = [Lot("L2", "SKU1", 10, expiry=D(2026, 6, 1), received=D(2026, 1, 5)),
                Lot("L1", "SKU1", 10, expiry=D(2026, 5, 1), received=D(2026, 1, 9))]
        picks = allocate("SKU1", lots, 15, D(2026, 3, 1))
        self.assertEqual([(a.lot_id, a.qty) for a in picks], [("L1", 10), ("L2", 5)])

    def test_lot_expiring_on_ship_date_is_not_used(self):
        lots = [Lot("L-77", "SKU1", 50, expiry=D(2026, 3, 10), received=D(2026, 1, 1)),
                Lot("L-80", "SKU1", 50, expiry=D(2026, 4, 30), received=D(2026, 2, 1))]
        picks = allocate("SKU1", lots, 20, D(2026, 3, 10))
        self.assertEqual([(a.lot_id, a.qty) for a in picks], [("L-80", 20)])

    def test_shortfall(self):
        lots = [Lot("L1", "SKU1", 5, expiry=D(2026, 5, 1))]
        with self.assertRaises(AllocationError) as ctx:
            allocate("SKU1", lots, 8, D(2026, 3, 1))
        self.assertEqual(ctx.exception.shortfall, 3)


if __name__ == "__main__":
    unittest.main()
