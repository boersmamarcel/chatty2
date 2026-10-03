import datetime
import unittest

from stockroom.allocation import AllocationError, allocate, eligible_lots, fefo_order
from stockroom.lots import Lot

D = datetime.date


def ids(lots):
    return [lot.lot_id for lot in lots]


class Issue1Test(unittest.TestCase):
    def test_expiry_on_ship_date_not_eligible(self):
        lots = [Lot("L-77", "S", 50, expiry=D(2026, 3, 10), received=D(2026, 1, 1)),
                Lot("L-80", "S", 50, expiry=D(2026, 3, 11), received=D(2026, 2, 1)),
                Lot("L-70", "S", 50, expiry=D(2026, 3, 9), received=D(2026, 1, 1))]
        self.assertEqual(ids(eligible_lots(lots, D(2026, 3, 10))), ["L-80"])
        picks = allocate("S", lots, 20, D(2026, 3, 10))
        self.assertEqual([(a.lot_id, a.qty) for a in picks], [("L-80", 20)])

    def test_expiry_on_ship_date_with_min_remaining_zero(self):
        lots = [Lot("L1", "S", 5, expiry=D(2026, 3, 10))]
        with self.assertRaises(AllocationError) as ctx:
            allocate("S", lots, 1, D(2026, 3, 10), min_remaining_days=0)
        self.assertEqual(ctx.exception.shortfall, 1)

    def test_min_remaining_days_unchanged(self):
        lots = [Lot("L1", "S", 5, expiry=D(2026, 3, 20)),
                Lot("L2", "S", 5, expiry=D(2026, 3, 19))]
        self.assertEqual(ids(eligible_lots(lots, D(2026, 3, 10), min_remaining_days=10)), ["L1"])
        self.assertEqual(ids(eligible_lots(lots, D(2026, 3, 10), min_remaining_days=9)),
                         ["L1", "L2"])

    def test_same_expiry_older_receipt_first(self):
        lots = [Lot("A1", "S", 10, expiry=D(2026, 5, 1), received=D(2026, 2, 20)),
                Lot("B9", "S", 10, expiry=D(2026, 5, 1), received=D(2026, 2, 1)),
                Lot("C5", "S", 10, expiry=D(2026, 4, 1), received=D(2026, 3, 1))]
        self.assertEqual(ids(fefo_order(lots)), ["C5", "B9", "A1"])
        picks = allocate("S", lots, 25, D(2026, 3, 15))
        self.assertEqual([(a.lot_id, a.qty) for a in picks], [("C5", 10), ("B9", 10), ("A1", 5)])

    def test_same_expiry_same_receipt_by_lot_id(self):
        lots = [Lot("L3", "S", 1, expiry=D(2026, 5, 1), received=D(2026, 2, 1)),
                Lot("L1", "S", 1, expiry=D(2026, 5, 1), received=D(2026, 2, 1))]
        self.assertEqual(ids(fefo_order(lots)), ["L1", "L3"])

    def test_missing_received_date_sorts_after(self):
        lots = [Lot("A", "S", 1, expiry=D(2026, 5, 1), received=None),
                Lot("B", "S", 1, expiry=D(2026, 5, 1), received=D(2026, 2, 1))]
        self.assertEqual(ids(fefo_order(lots)), ["B", "A"])

    def test_lots_without_expiry_last(self):
        lots = [Lot("N2", "S", 1, received=None),
                Lot("N1", "S", 1, received=D(2026, 2, 5)),
                Lot("N0", "S", 1, received=D(2026, 1, 5)),
                Lot("E1", "S", 1, expiry=D(2027, 1, 1), received=D(2026, 3, 1)),
                Lot("N3", "S", 1, received=None)]
        self.assertEqual(ids(fefo_order(lots)), ["E1", "N0", "N1", "N2", "N3"])

    def test_shortfall_reported(self):
        lots = [Lot("L1", "S", 5, expiry=D(2026, 5, 1)),
                Lot("L2", "S", 7, expiry=D(2026, 3, 1))]
        with self.assertRaises(AllocationError) as ctx:
            allocate("S", lots, 9, D(2026, 3, 1))
        self.assertEqual(ctx.exception.shortfall, 4)


if __name__ == "__main__":
    unittest.main()
