import io
import os
import shutil
import tempfile
import unittest

from transit.cli import main
from transit.model import Network
from transit.validate import validate_network


def make(stops, links):
    net = Network()
    for stop_id in stops:
        net.add_stop(stop_id, "Stop " + stop_id)
    for a, b, oneway in links:
        net.add_link(a, b, "L", 1, oneway=oneway)
    return net


BAD_FILE = """\
[stops]
id, name, zone
A, Alpha, 1
B, Bravo, 1
C, Charlie, 1
D, Delta, 1
E, Echo, 1
G, Golf, 1

[links]
from, to, line, minutes, oneway
A, B, L, 2
C, A, L, 2, yes
B, D, L, 2, yes
G, D, L, 2, yes
"""

GOOD_FILE = """\
[stops]
id, name, zone
A, Alpha, 1
B, Bravo, 1
C, Charlie, 1

[links]
from, to, line, minutes, oneway
A, B, L, 2
B, C, L, 2, yes
C, A, L, 2, yes
"""


class ValidateTest(unittest.TestCase):

    def test_oneway_respected_and_sorted(self):
        net = make("ABCDEG", [("A", "B", False), ("C", "A", True), ("B", "D", True),
                              ("G", "D", True)])
        self.assertEqual(validate_network(net), [
            "stop 'C' cannot be reached from 'A'",
            "isolated stop 'E'",
            "stop 'G' cannot be reached from 'A'",
        ])

    def test_isolated_not_reported_twice(self):
        net = make("ABZ", [("A", "B", False)])
        self.assertEqual(validate_network(net), ["isolated stop 'Z'"])

    def test_isolated_sorted_with_others(self):
        net = make("ABKXY", [("B", "X", False), ("A", "X", True)])
        # root is A (smallest id with a link); B is reachable via X; K, Y isolated
        self.assertEqual(validate_network(net), ["isolated stop 'K'", "isolated stop 'Y'"])

    def test_root_is_smallest_linked_stop(self):
        net = make("ABCD", [("B", "C", True), ("D", "C", False)])
        # A is isolated, so the root is B; D is reachable B -> C -> D.
        self.assertEqual(validate_network(net), ["isolated stop 'A'"])
        net = make("ABCD", [("C", "B", True), ("D", "C", False)])
        self.assertEqual(validate_network(net), [
            "isolated stop 'A'",
            "stop 'C' cannot be reached from 'B'",
            "stop 'D' cannot be reached from 'B'",
        ])

    def test_valid_cycle(self):
        net = make("ABC", [("A", "B", True), ("B", "C", True), ("C", "A", True)])
        self.assertEqual(validate_network(net), [])


class CheckCommandTest(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.mkdtemp()

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def write(self, text):
        path = os.path.join(self.tmp, "net.txt")
        with io.open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        return path

    def run_check(self, path):
        out, err = io.StringIO(), io.StringIO()
        status = main(["check", path], out=out, err=err)
        return status, out.getvalue(), err.getvalue()

    def test_problems_exit_1(self):
        status, out, _err = self.run_check(self.write(BAD_FILE))
        self.assertEqual(status, 1)
        self.assertEqual(out.splitlines(), [
            "stop 'C' cannot be reached from 'A'",
            "isolated stop 'E'",
            "stop 'G' cannot be reached from 'A'",
        ])

    def test_ok_exit_0(self):
        status, out, _err = self.run_check(self.write(GOOD_FILE))
        self.assertEqual(status, 0)
        self.assertEqual(out, "OK: 3 stops, 3 links\n")

    def test_load_error_exit_2(self):
        status, out, err = self.run_check(os.path.join(self.tmp, "missing.txt"))
        self.assertEqual(status, 2)
        self.assertTrue(err.startswith("error: "))


if __name__ == "__main__":
    unittest.main()
