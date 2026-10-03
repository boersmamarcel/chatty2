"""Picking versions from the list of published versions of a package."""

from .constraints import Constraint, parse_constraint
from .version import Version, VersionError


def _as_version(value):
    if isinstance(value, Version):
        return value
    return Version.parse(value)


def sort_versions(versions, reverse=False):
    """Versions (strings or Version) sorted by version order, as Version."""
    return sorted((_as_version(v) for v in versions), reverse=reverse)


def max_satisfying(versions, constraint, include_prerelease=False):
    """The highest published version that satisfies `constraint`.

    `versions` holds version strings or Version objects as published by
    the registry. Returns a Version, or None when nothing matches.
    """
    candidates = [_as_version(v) for v in versions]
    matching = [v for v in candidates if constraint.matches(v)]
    if not matching:
        return None
    return max(matching)


def min_satisfying(versions, constraint):
    """The lowest published release version that satisfies `constraint`."""
    if isinstance(constraint, str):
        constraint = parse_constraint(constraint)
    matching = []
    for value in versions:
        try:
            version = _as_version(value)
        except VersionError:
            continue
        if not version.is_prerelease and constraint.matches(version):
            matching.append(version)
    return min(matching) if matching else None


def outdated(pins, published):
    """Packages whose pinned version is lower than the newest release.

    `pins` maps name -> Version, `published` maps name -> list of versions.
    Returns a sorted list of (name, pinned, newest).
    """
    rows = []
    for name in sorted(pins):
        newest = max_satisfying(published.get(name, []), Constraint([], "*"))
        if newest is not None and newest > pins[name]:
            rows.append((name, pins[name], newest))
    return rows
