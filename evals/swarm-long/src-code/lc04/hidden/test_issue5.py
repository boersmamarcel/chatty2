"""Issue 5: round and filesizeformat."""

import unittest

from stencil import Environment
from stencil.errors import FilterArgumentError
from stencil.numbers import filesizeformat, round_value


def render(source, **variables):
    return Environment().from_string(source).render(**variables)


class RoundCommonTests(unittest.TestCase):

    def test_halves_round_up(self):
        self.assertEqual(round_value(2.5), 3.0)
        self.assertEqual(round_value(3.5), 4.0)
        self.assertEqual(round_value(0.5), 1.0)
        self.assertEqual(round_value(2.4), 2.0)

    def test_negative_halves_away_from_zero(self):
        self.assertEqual(round_value(-2.5), -3.0)
        self.assertEqual(round_value(-0.5), -1.0)

    def test_decimal_value_of_float(self):
        self.assertEqual(round_value(2.675, 2), 2.68)
        self.assertEqual(round_value(1.005, 2), 1.01)
        self.assertEqual(round_value(0.125, 2), 0.13)

    def test_returns_float(self):
        self.assertIsInstance(round_value(7), float)
        self.assertEqual(render("{{ 7|round }}"), "7.0")

    def test_numeric_string(self):
        self.assertEqual(round_value("2.5"), 3.0)

    def test_filter(self):
        self.assertEqual(render("{{ x|round }} {{ y|round(2) }}", x=-2.5, y=2.675), "-3.0 2.68")


class RoundCeilFloorTests(unittest.TestCase):

    def test_exact_on_decimal_value(self):
        self.assertEqual(round_value(1.1, 2, "ceil"), 1.1)
        self.assertEqual(round_value(0.29, 2, "floor"), 0.29)

    def test_directions(self):
        self.assertEqual(round_value(-2.71, 1, "ceil"), -2.7)
        self.assertEqual(round_value(-2.71, 1, "floor"), -2.8)
        self.assertEqual(round_value(2.71, 1, "ceil"), 2.8)
        self.assertEqual(round_value(2.79, 1, "floor"), 2.7)

    def test_filter_with_method(self):
        self.assertEqual(render("{{ x|round(2, 'floor') }}", x=0.29), "0.29")

    def test_unknown_method(self):
        with self.assertRaises(FilterArgumentError):
            round_value(1.5, 0, "banker")


class FileSizeTests(unittest.TestCase):

    def test_bytes(self):
        self.assertEqual(filesizeformat(1), "1 Byte")
        self.assertEqual(filesizeformat(0), "0 Bytes")
        self.assertEqual(filesizeformat(999), "999 Bytes")

    def test_unit_boundaries_inclusive(self):
        self.assertEqual(filesizeformat(1000), "1.0 kB")
        self.assertEqual(filesizeformat(1000000), "1.0 MB")
        self.assertEqual(filesizeformat(10 ** 9), "1.0 GB")

    def test_binary(self):
        self.assertEqual(filesizeformat(1023, True), "1023 Bytes")
        self.assertEqual(filesizeformat(1024, True), "1.0 KiB")
        self.assertEqual(filesizeformat(1536, True), "1.5 KiB")
        self.assertEqual(filesizeformat(1048576, True), "1.0 MiB")

    def test_half_up(self):
        self.assertEqual(filesizeformat(1250), "1.3 kB")
        self.assertEqual(filesizeformat(1150), "1.2 kB")
        self.assertEqual(filesizeformat(2500000), "2.5 MB")

    def test_filter(self):
        self.assertEqual(render("{{ n|filesizeformat }}|{{ n|filesizeformat(true) }}", n=1000000),
                         "1.0 MB|976.6 KiB")


if __name__ == "__main__":
    unittest.main()
