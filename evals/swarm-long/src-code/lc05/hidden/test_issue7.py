import unittest
from decimal import Decimal

from throttle.metrics import Metrics, deny_percent
from throttle.report import render


def feed(metrics, tenant, allowed, denied):
    for _ in range(allowed):
        metrics.record(tenant, True)
    for _ in range(denied):
        metrics.record(tenant, False)


class Issue7DenyPercentTest(unittest.TestCase):
    def test_half_up(self):
        cases = [((15, 1), "6.3"), ((79, 1), "1.3"), ((1, 2), "66.7"), ((2, 1), "33.3"),
                 ((7, 1), "12.5"), ((399, 1), "0.3"), ((0, 4), "100.0"), ((5, 0), "0.0")]
        for (allowed, denied), expected in cases:
            got = deny_percent(allowed, denied)
            self.assertIsInstance(got, Decimal)
            self.assertEqual(str(got), expected, (allowed, denied))

    def test_no_decisions(self):
        got = deny_percent(0, 0)
        self.assertIsInstance(got, Decimal)
        self.assertEqual(str(got), "0.0")

    def test_deny_rate(self):
        metrics = Metrics()
        feed(metrics, "acme", 15, 1)
        self.assertEqual(metrics.deny_rate("acme"), Decimal("6.3"))
        self.assertEqual(str(metrics.deny_rate("unknown")), "0.0")


class Issue7ReportTest(unittest.TestCase):
    def test_rounded_values_in_table(self):
        metrics = Metrics()
        feed(metrics, "acme", 15, 1)
        feed(metrics, "globex", 79, 1)
        self.assertEqual(render(metrics), "\n".join([
            "tenant        allowed   denied   deny%",
            "acme               15        1     6.3",
            "globex             79        1     1.3",
            "TOTAL              94        2     2.1",
        ]) + "\n")

    def test_order_denied_then_percent_then_name(self):
        metrics = Metrics()
        feed(metrics, "delta", 6, 2)    # 25.0
        feed(metrics, "alpha", 2, 2)    # 50.0
        feed(metrics, "carol", 2, 2)    # 50.0
        feed(metrics, "bravo", 9, 3)    # 25.0, most denied
        feed(metrics, "echo", 1, 0)     # 0.0
        names = [line.split()[0] for line in render(metrics).splitlines()[1:-1]]
        self.assertEqual(names, ["bravo", "alpha", "carol", "delta", "echo"])

    def test_tie_on_rounded_percent_falls_back_to_name(self):
        metrics = Metrics()
        feed(metrics, "zed", 3999, 1)    # 0.025 -> 0.0
        feed(metrics, "amy", 4001, 1)    # 0.0249... -> 0.0
        feed(metrics, "kim", 1999, 1)    # 0.05 -> 0.1
        names = [line.split()[0] for line in render(metrics).splitlines()[1:-1]]
        self.assertEqual(names, ["kim", "amy", "zed"])

    def test_top(self):
        metrics = Metrics()
        feed(metrics, "b", 2, 2)
        feed(metrics, "a", 2, 2)
        feed(metrics, "c", 2, 5)
        lines = render(metrics, top=2).splitlines()
        self.assertEqual([line.split()[0] for line in lines[1:]], ["c", "a", "TOTAL"])
        self.assertEqual(lines[-1], "TOTAL               6        9    60.0")


if __name__ == "__main__":
    unittest.main()
