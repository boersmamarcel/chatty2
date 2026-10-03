import unittest

from verspec.bump import bump
from verspec.version import Version


def b(version, part):
    return str(bump(version, part))


class BumpTest(unittest.TestCase):
    def test_release(self):
        self.assertEqual(b("1.2.3", "major"), "2.0.0")
        self.assertEqual(b("1.2.3", "minor"), "1.3.0")
        self.assertEqual(b("1.2.3", "patch"), "1.2.4")
        self.assertEqual(b("0.0.0", "minor"), "0.1.0")

    def test_prerelease_of_part(self):
        self.assertEqual(b("1.2.3-rc.1", "patch"), "1.2.3")
        self.assertEqual(b("1.3.0-rc.2", "minor"), "1.3.0")
        self.assertEqual(b("2.0.0-rc.1", "major"), "2.0.0")
        self.assertEqual(b("2.0.0-alpha", "minor"), "2.0.0")
        self.assertEqual(b("2.0.0-alpha", "patch"), "2.0.0")

    def test_prerelease_of_lower_part(self):
        self.assertEqual(b("1.2.3-rc.1", "minor"), "1.3.0")
        self.assertEqual(b("1.2.3-rc.1", "major"), "2.0.0")
        self.assertEqual(b("1.3.0-rc.2", "major"), "2.0.0")
        self.assertEqual(b("1.0.1-rc.2", "major"), "2.0.0")

    def test_build_dropped(self):
        self.assertEqual(b("1.2.3+b.5", "patch"), "1.2.4")
        self.assertEqual(b("1.2.3+b.5", "minor"), "1.3.0")
        self.assertEqual(b("1.2.3-rc.1+b.5", "patch"), "1.2.3")
        result = bump(Version.parse("1.3.0-rc.2+b.1"), "minor")
        self.assertEqual(result.prerelease, ())
        self.assertEqual(result.build, ())

    def test_prerelease_unchanged(self):
        self.assertEqual(b("1.2.3-rc.1", "prerelease"), "1.2.3-rc.2")
        self.assertEqual(b("1.2.3-rc", "prerelease"), "1.2.3-rc.0")

    def test_unknown(self):
        with self.assertRaises(ValueError):
            bump("1.2.3", "build")


if __name__ == "__main__":
    unittest.main()
