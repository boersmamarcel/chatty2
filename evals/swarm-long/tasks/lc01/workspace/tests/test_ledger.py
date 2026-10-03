import datetime
import unittest

from stockroom.ledger import LedgerError, StockLedger, make_movement

D = datetime.date


class LedgerTest(unittest.TestCase):
    def setUp(self):
        self.ledger = StockLedger()
        self.ledger.post(make_movement("M1", D(2026, 3, 1), "sku1", "RECEIPT", 100, lot="A"))
        self.ledger.post(make_movement("M2", D(2026, 3, 2), "SKU1", "PICK", 30, lot="A"))

    def test_on_hand(self):
        self.assertEqual(self.ledger.on_hand("SKU1"), 70)
        self.assertEqual(self.ledger.on_hand("SKU1", as_of=D(2026, 3, 1)), 100)

    def test_negative_stock_refused(self):
        with self.assertRaises(LedgerError):
            self.ledger.post(make_movement("M3", D(2026, 3, 3), "SKU1", "PICK", 71, lot="A"))

    def test_duplicate_id_refused(self):
        with self.assertRaises(LedgerError):
            self.ledger.post(make_movement("M1", D(2026, 3, 3), "SKU1", "RECEIPT", 1))


if __name__ == "__main__":
    unittest.main()
