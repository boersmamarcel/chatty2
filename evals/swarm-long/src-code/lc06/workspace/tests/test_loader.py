import unittest

from transit.errors import NetworkFormatError
from transit.loader import load_network

BASE = """\
# Harbour city trams
[stops]
id, name, zone
CEN, Central, 1
MUS, Museum, 1
HAR, "Harbour, North", 2

[links]
from, to, line, minutes, oneway
{links}
"""


class LoaderTest(unittest.TestCase):

    def test_basic_file(self):
        net = load_network(BASE.format(links="CEN, MUS, T1, 4\nMUS, HAR, T1, 6, yes"))
        self.assertEqual(net.stop_ids(), ["CEN", "HAR", "MUS"])
        self.assertEqual(net.name("HAR"), "Harbour, North")
        self.assertEqual(net.neighbours("MUS"), ["CEN", "HAR"])
        self.assertEqual(net.neighbours("HAR"), [])
        self.assertEqual(net.link_count, 2)

    def test_bad_minutes_reported_with_line_number(self):
        with self.assertRaises(NetworkFormatError) as ctx:
            load_network(BASE.format(links="CEN, MUS, T1, 0"))
        self.assertEqual(ctx.exception.errors,
                         ["line 10: minutes must be a positive integer, got '0'"])

    def test_link_ids_are_case_insensitive(self):
        net = load_network(BASE.format(links="cen, mus, T1, 4\n Mus ,har, T1, 6"))
        self.assertEqual(net.neighbours("CEN"), ["MUS"])
        self.assertEqual(net.neighbours("MUS"), ["CEN", "HAR"])


if __name__ == "__main__":
    unittest.main()
