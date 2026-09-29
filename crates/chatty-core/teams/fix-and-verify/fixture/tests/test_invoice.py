import unittest

from invoice import invoice_total, line_total


class TestInvoice(unittest.TestCase):
    def test_line_total_applies_the_discount(self):
        self.assertEqual(line_total(20.00, 3, discount_pct=10), 54.00)

    def test_small_order_pays_shipping(self):
        self.assertEqual(invoice_total([{"unit_price": 12.50, "quantity": 2}]), 29.95)

    def test_large_order_ships_free(self):
        self.assertEqual(invoice_total([{"unit_price": 30.00, "quantity": 2}]), 60.00)

    def test_order_of_exactly_the_threshold_ships_free(self):
        self.assertEqual(invoice_total([{"unit_price": 25.00, "quantity": 2}]), 50.00)


if __name__ == "__main__":
    unittest.main()
