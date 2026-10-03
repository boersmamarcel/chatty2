import unittest

from verspec.constraints import ConstraintError, parse_constraint
from verspec.lockfile import diff_locks, dump_lock
from verspec.resolve import min_satisfying
from verspec.version import Version


def ok(constraint, version):
    return parse_constraint(constraint).matches(Version.parse(version))


class ConstraintTest(unittest.TestCase):
    def test_ranges(self):
        self.assertTrue(ok(">=1.2.0, <2.0.0", "1.9.9"))
        self.assertFalse(ok(">=1.2.0, <2.0.0", "2.0.0"))
        self.assertTrue(ok("!=1.2.4", "1.2.5"))

    def test_caret_and_tilde(self):
        self.assertTrue(ok("^1.2.3", "1.9.0"))
        self.assertFalse(ok("^1.2.3", "2.0.0"))
        self.assertTrue(ok("~1.2.3", "1.2.9"))
        self.assertFalse(ok("~1.2.3", "1.3.0"))

    def test_wildcards(self):
        self.assertTrue(ok("1.2.x", "1.2.7"))
        self.assertFalse(ok("1.x", "2.0.0"))
        self.assertTrue(ok("*", "0.0.1"))

    def test_errors(self):
        for bad in ["", ">=1.2", "^x", ">=1.0.0,"]:
            with self.assertRaises(ConstraintError, msg=bad):
                parse_constraint(bad)

    def test_min_satisfying(self):
        self.assertEqual(str(min_satisfying(["2.0.0", "1.4.0", "1.5.0"], ">=1.4.1")), "1.5.0")


class LockToolsTest(unittest.TestCase):
    def test_dump_and_diff(self):
        old = {"b": Version.parse("1.0.0"), "a": Version.parse("2.0.0")}
        new = {"a": Version.parse("2.1.0"), "c": Version.parse("0.1.0")}
        self.assertEqual(dump_lock(old, header=""), "a==2.0.0\nb==1.0.0\n")
        diff = diff_locks(old, new)
        self.assertEqual([n for n, _ in diff["added"]], ["c"])
        self.assertEqual([n for n, _ in diff["removed"]], ["b"])
        self.assertEqual([n for n, _, _ in diff["changed"]], ["a"])


if __name__ == "__main__":
    unittest.main()
