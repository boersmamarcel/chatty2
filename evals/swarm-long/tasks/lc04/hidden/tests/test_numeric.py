"""Numeric filters."""

import unittest

from stencil import Environment


def render(source, **variables):
    return Environment().from_string(source).render(**variables)


class NumericFilterTests(unittest.TestCase):

    def test_round_half_up(self):
        self.assertEqual(render("{{ 2.5|round }}"), "3.0")
        self.assertEqual(render("{{ 2.4|round }}"), "2.0")

    def test_round_precision(self):
        self.assertEqual(render("{{ 3.14159|round(2) }}"), "3.14")

    def test_filesize_unit_boundary(self):
        self.assertEqual(render("{{ 1000|filesizeformat }}"), "1.0 kB")
        self.assertEqual(render("{{ 999|filesizeformat }}"), "999 Bytes")

    def test_numberformat(self):
        self.assertEqual(render("{{ 1234567.891|numberformat }}"), "1,234,567.89")


if __name__ == "__main__":
    unittest.main()
