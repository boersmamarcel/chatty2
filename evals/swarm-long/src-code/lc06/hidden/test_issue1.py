import unittest

from transit.errors import NetworkFormatError
from transit.loader import load_network


def doc(stops, links="", transfers=None):
    text = "[stops]\nid, name, zone\n" + stops + "\n[links]\nfrom, to, line, minutes, oneway\n" + links + "\n"
    if transfers is not None:
        text += "[transfers]\nstop, minutes\n" + transfers + "\n"
    return text


STOPS = "CEN, Central, 1\nMUS, Museum, 1\nHAR, Harbour, 2"


class Issue1Test(unittest.TestCase):

    def test_links_normalised(self):
        net = load_network(doc(STOPS, "cen, mus, T1, 4\n  Mus ,  hAr , T1, 6, yes"))
        self.assertEqual(net.stop_ids(), ["CEN", "HAR", "MUS"])
        self.assertEqual([(l.a, l.b, l.line, l.minutes) for l in net.outgoing("CEN")],
                         [("CEN", "MUS", "T1", 4)])
        self.assertEqual(net.neighbours("MUS"), ["CEN", "HAR"])
        self.assertEqual(net.neighbours("HAR"), [])
        self.assertEqual([(r.a, r.b) for r in net.records], [("CEN", "MUS"), ("MUS", "HAR")])

    def test_stop_ids_in_stops_section_normalised(self):
        net = load_network(doc(" cen , Central, 1\nmus, Museum, 1", "CEN, MUS, T1, 4"))
        self.assertEqual(net.stop_ids(), ["CEN", "MUS"])
        self.assertEqual(net.name("CEN"), "Central")

    def test_transfers_normalised(self):
        net = load_network(doc(STOPS, "CEN, MUS, T1, 4", " mus , 3\nhar, 0"))
        self.assertEqual(net.transfer_time("MUS"), 3)
        self.assertEqual(net.transfer_time("HAR"), 0)
        self.assertEqual(net.transfer_time("CEN"), 0)

    def test_unknown_stop_message_uses_canonical_id(self):
        with self.assertRaises(NetworkFormatError) as ctx:
            load_network(doc(STOPS, "cen, mus, T1, 4\ncen, xyz, T1, 5"))
        self.assertEqual(ctx.exception.errors, ["line 9: unknown stop 'XYZ'"])

    def test_unknown_transfer_stop_canonical(self):
        with self.assertRaises(NetworkFormatError) as ctx:
            load_network(doc(STOPS, "CEN, MUS, T1, 4", "qq, 2"))
        self.assertEqual(ctx.exception.errors, ["line 11: unknown stop 'QQ'"])

    def test_line_names_are_case_sensitive(self):
        net = load_network(doc(STOPS, "cen, mus, l1, 4\nmus, har, L1, 6\nhar, cen,  T2 , 9"))
        self.assertEqual(net.lines(), ["L1", "T2", "l1"])
        self.assertEqual(net.outgoing("CEN")[1].line, "l1")

    def test_duplicate_after_normalisation(self):
        text = doc("A, First, 1\nB, Bee, 1\n a , Second, 2", "A, B, L1, 3")
        with self.assertRaises(NetworkFormatError) as ctx:
            load_network(text)
        self.assertEqual(ctx.exception.errors, ["line 5: duplicate stop 'A'"])

    def test_several_duplicates(self):
        text = doc("a, First, 1\nA, Second, 2\nb, Bee, 1\nB , Bee again, 1", "a, b, L1, 3")
        with self.assertRaises(NetworkFormatError) as ctx:
            load_network(text)
        self.assertEqual(ctx.exception.errors,
                         ["line 4: duplicate stop 'A'", "line 6: duplicate stop 'B'"])

    def test_errors_collected_in_line_order(self):
        text = doc(STOPS + "\ncen, Again, 1",
                   "cen, nope, T1, 4\nmus, har, T1, -2\nhar, mus, T1, 3, maybe",
                   "Zed, 1")
        with self.assertRaises(NetworkFormatError) as ctx:
            load_network(text)
        self.assertEqual(ctx.exception.errors, [
            "line 6: duplicate stop 'CEN'",
            "line 9: unknown stop 'NOPE'",
            "line 10: minutes must be a positive integer, got '-2'",
            "line 11: bad oneway flag 'maybe'",
            "line 14: unknown stop 'ZED'",
        ])


if __name__ == "__main__":
    unittest.main()
