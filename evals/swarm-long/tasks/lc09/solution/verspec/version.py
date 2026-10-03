"""Semantic versions (Semantic Versioning 2.0.0).

    MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]

PRERELEASE and BUILD are dot-separated identifiers of ASCII letters,
digits and hyphens. A leading `v` (`v1.2.3`) is accepted and dropped.

Ordering follows section 11 of the specification:

* major, minor and patch are compared numerically;
* a pre-release version is lower than the release with the same
  major.minor.patch (1.0.0-rc.1 < 1.0.0);
* two pre-releases are compared identifier by identifier, left to right:
  identifiers of digits only are compared numerically, others in ASCII
  order, and a numeric identifier is always lower than a non-numeric one;
  if all identifiers so far are equal, the version with fewer identifiers
  is lower (1.0.0-alpha < 1.0.0-alpha.1);
* build metadata is ignored for ordering and equality.
"""

import functools
import re

_VERSION_RE = re.compile(
    r"^v?(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$")


class VersionError(ValueError):
    """Raised for text that is not a valid version."""


def _identifiers(text):
    """Split a pre-release or build string into its identifiers."""
    if not text:
        return ()
    return tuple(text.split("."))


@functools.total_ordering
class Version(object):
    """An immutable semantic version."""

    __slots__ = ("major", "minor", "patch", "prerelease", "build")

    def __init__(self, major, minor, patch, prerelease=(), build=()):
        for number in (major, minor, patch):
            if not isinstance(number, int) or number < 0:
                raise VersionError("version numbers must be non-negative ints")
        self.major = major
        self.minor = minor
        self.patch = patch
        self.prerelease = tuple(prerelease)
        self.build = tuple(build)

    @classmethod
    def parse(cls, text):
        """Parse a version string; raises VersionError."""
        if not isinstance(text, str):
            raise VersionError("not a string: %r" % (text,))
        match = _VERSION_RE.match(text.strip())
        if not match:
            raise VersionError("invalid version %r" % (text,))
        major, minor, patch, pre, build = match.groups()
        pre_ids = _identifiers(pre)
        build_ids = _identifiers(build)
        if any(not ident for ident in pre_ids + build_ids):
            raise VersionError("empty identifier in %r" % (text,))
        for ident in pre_ids:
            if ident.isdigit() and len(ident) > 1 and ident.startswith("0"):
                raise VersionError("leading zero in pre-release identifier of %r" % (text,))
        return cls(int(major), int(minor), int(patch), pre_ids, build_ids)

    @property
    def is_prerelease(self):
        return bool(self.prerelease)

    def release(self):
        """The same version without pre-release and build parts."""
        return Version(self.major, self.minor, self.patch)

    def _key(self):
        if not self.prerelease:
            pre = (1,)
        else:
            pre = (0, tuple((0, int(ident), "") if ident.isdigit() else (1, 0, ident)
                            for ident in self.prerelease))
        return (self.major, self.minor, self.patch, pre)

    def __eq__(self, other):
        if not isinstance(other, Version):
            return NotImplemented
        return self._key() == other._key()

    def __lt__(self, other):
        if not isinstance(other, Version):
            return NotImplemented
        return self._key() < other._key()

    def __hash__(self):
        return hash(self._key())

    def __str__(self):
        text = "%d.%d.%d" % (self.major, self.minor, self.patch)
        if self.prerelease:
            text += "-" + ".".join(self.prerelease)
        if self.build:
            text += "+" + ".".join(self.build)
        return text

    def __repr__(self):
        return "Version(%r)" % str(self)


def parse(text):
    """Shortcut for Version.parse."""
    return Version.parse(text)
