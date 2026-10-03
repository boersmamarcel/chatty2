import unittest

from verspec.constraints import ConstraintError, parse_constraint
from verspec.resolve import max_satisfying

PUBLISHED = ["1.2.0", "1.3.0-beta.1", "1.2.5", "latest", "2.0.0", "", "1.2.7-rc.1", "next"]


class MaxSatisfyingTest(unittest.TestCase):
    def test_no_prereleases_by_default(self):
        self.assertEqual(str(max_satisfying(PUBLISHED, parse_constraint("^1.2.0"))), "1.2.5")
        self.assertEqual(str(max_satisfying(PUBLISHED, parse_constraint(">=1.0.0"))), "2.0.0")
        self.assertIsNone(max_satisfying(["1.3.0-beta.1"], parse_constraint("^1.2.0")))

    def test_include_prerelease(self):
        self.assertEqual(str(max_satisfying(PUBLISHED, parse_constraint("^1.2.0"),
                                            include_prerelease=True)), "1.3.0-beta.1")
        self.assertEqual(str(max_satisfying(PUBLISHED, "<1.2.9", True)), "1.2.7-rc.1")

    def test_string_constraint(self):
        self.assertEqual(str(max_satisfying(PUBLISHED, "^1.2.0")), "1.2.5")
        self.assertEqual(str(max_satisfying(PUBLISHED, "<1.2.5")), "1.2.0")
        self.assertIsNone(max_satisfying(PUBLISHED, ">2.0.0"))
        with self.assertRaises(ConstraintError):
            max_satisfying(PUBLISHED, ">=1.2")

    def test_junk_only(self):
        self.assertIsNone(max_satisfying(["latest", "x.y.z"], "*"))
        self.assertIsNone(max_satisfying([], "*"))

    def test_build_metadata_first_listed(self):
        best = max_satisfying(["1.0.0+b.2", "0.9.0", "1.0.0+b.1"], "^1.0.0")
        self.assertEqual(str(best), "1.0.0+b.2")


if __name__ == "__main__":
    unittest.main()
