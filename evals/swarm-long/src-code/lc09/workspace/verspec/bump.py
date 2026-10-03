"""Computing the next version.

`bump(version, part)` answers "what is the next MAJOR / MINOR / PATCH /
pre-release version after this one". The rules follow common practice
(npm version, poetry version):

* bumping a release increments that part and resets the parts after it:
  1.2.3 -> major 2.0.0, minor 1.3.0, patch 1.2.4;
* a pre-release is "on its way" to its release, so bumping the part it is
  a pre-release of just drops the pre-release:
  1.2.3-rc.1 -> patch 1.2.3, 1.3.0-rc.1 -> minor 1.3.0,
  2.0.0-rc.1 -> major 2.0.0;
* build metadata is always dropped.

The pre-release bump increments the last numeric identifier or appends
`.0`: 1.2.3-rc.1 -> 1.2.3-rc.2, 1.2.3-rc -> 1.2.3-rc.0, and on a release it
starts the next patch's pre-release: 1.2.3 -> 1.2.4-<preid>.0.
"""

from .version import Version

PARTS = ("major", "minor", "patch", "prerelease")


def _bump_prerelease(version, preid):
    if not version.prerelease:
        return Version(version.major, version.minor, version.patch + 1, (preid, "0"))
    identifiers = list(version.prerelease)
    for index in range(len(identifiers) - 1, -1, -1):
        if identifiers[index].isdigit():
            identifiers[index] = str(int(identifiers[index]) + 1)
            break
    else:
        identifiers.append("0")
    return Version(version.major, version.minor, version.patch, identifiers)


def bump(version, part, preid="rc"):
    """The next version; `version` is a Version or a version string."""
    if isinstance(version, str):
        version = Version.parse(version)
    if part not in PARTS:
        raise ValueError("unknown part %r, expected one of %s" % (part, ", ".join(PARTS)))
    if part == "major":
        return Version(version.major + 1, 0, 0)
    if part == "minor":
        return Version(version.major, version.minor + 1, version.patch, version.prerelease)
    if part == "patch":
        return Version(version.major, version.minor, version.patch + 1)
    return _bump_prerelease(version, preid)
