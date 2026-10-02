"""Compare semantic versions: MAJOR.MINOR.PATCH with an optional -prerelease."""


def parse(version):
    core, _, pre = version.partition("-")
    return core.split("."), pre


def compare(a, b):
    """-1, 0 or 1. A prerelease sorts before its release (1.0.0-rc1 < 1.0.0)."""
    (ca, pa), (cb, pb) = parse(a), parse(b)
    if ca != cb:
        return -1 if ca < cb else 1
    if pa == pb:
        return 0
    return -1 if pa < pb else 1
