import unittest

from verspec.version import Version, VersionError


class ParseTest(unittest.TestCase):
    def test_parse(self):
        v = Version.parse("v1.2.3-rc.1+build.5")
        self.assertEqual((v.major, v.minor, v.patch), (1, 2, 3))
        self.assertEqual(v.prerelease, ("rc", "1"))
        self.assertEqual(str(v), "1.2.3-rc.1+build.5")
        with self.assertRaises(VersionError):
            Version.parse("1.2")


class OrderTest(unittest.TestCase):
    def test_release_order(self):
        self.assertLess(Version.parse("1.9.0"), Version.parse("1.10.0"))
        self.assertLess(Version.parse("1.0.0-rc.1"), Version.parse("1.0.0"))

    def test_numeric_prerelease_identifiers(self):
        self.assertLess(Version.parse("1.0.0-rc.9"), Version.parse("1.0.0-rc.10"))
        self.assertLess(Version.parse("1.0.0-beta.2"), Version.parse("1.0.0-beta.11"))


if __name__ == "__main__":
    unittest.main()
