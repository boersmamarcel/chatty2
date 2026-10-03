import unittest

from transit.errors import UnknownStopError
from transit.model import Network, count_changes, route_from_path


class ModelTest(unittest.TestCase):

    def setUp(self):
        self.net = Network()
        for stop_id in "ABC":
            self.net.add_stop(stop_id, "Stop " + stop_id, "1")
        self.net.add_link("A", "B", "L1", 3)
        self.net.add_link("B", "C", "L2", 4, oneway=True)

    def test_count_changes(self):
        self.assertEqual(count_changes([]), 0)
        self.assertEqual(count_changes(["L1"]), 0)
        self.assertEqual(count_changes(["L1", "L1", "L2", "L1"]), 2)

    def test_oneway(self):
        self.assertEqual(self.net.neighbours("B"), ["A", "C"])
        self.assertEqual(self.net.neighbours("C"), [])

    def test_unknown_stop(self):
        with self.assertRaises(UnknownStopError):
            self.net.stop("X")

    def test_route_from_path(self):
        route = route_from_path(self.net, ["A", "B", "C"], ["L1", "L2"], transfer_penalty=5)
        self.assertEqual(route.cost, 12)
        self.assertEqual(route.changes, 1)
        self.assertEqual(route.minutes, 7)


if __name__ == "__main__":
    unittest.main()
