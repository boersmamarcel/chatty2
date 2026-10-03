"""Version constraints.

A constraint is a comma-separated list of comparators that must all hold:

    >=1.2.0, <2.0.0

Comparators:

    =1.2.3  ==1.2.3  1.2.3     exactly this version
    !=1.2.3                    anything but this version
    >1.2.3  >=1.2.3  <1.2.3  <=1.2.3
    ^1.2.3   compatible:  >=1.2.3, <2.0.0
             with major 0 the first non-zero part is the "major":
             ^0.2.3 := >=0.2.3, <0.3.0   and   ^0.0.3 := >=0.0.3, <0.0.4
    ~1.2.3   patch-level: >=1.2.3, <1.3.0
    1.2.x  1.x  *          wildcards: >=1.2.0, <1.3.0 / >=1.0.0, <2.0.0 / any

Matching is plain version ordering (see `verspec.version`); whether
pre-releases are wanted is decided by the caller (`verspec.resolve`).
"""

import operator
import re

from .version import Version, VersionError


class ConstraintError(ValueError):
    """Raised for a malformed constraint."""


_OPS = {
    "=": operator.eq,
    "==": operator.eq,
    "!=": operator.ne,
    ">": operator.gt,
    ">=": operator.ge,
    "<": operator.lt,
    "<=": operator.le,
}

_COMPARATOR_RE = re.compile(r"^(==|!=|>=|<=|=|>|<|\^|~)?\s*(.+)$")
_WILDCARD_RE = re.compile(r"^(?:(0|[1-9][0-9]*)\.)?(?:(0|[1-9][0-9]*)\.)?[xX*]$")


class Comparator(object):
    """One `op version` condition."""

    def __init__(self, op, version):
        if op not in _OPS:
            raise ConstraintError("unknown operator %r" % (op,))
        self.op = op
        self.version = version

    def matches(self, version):
        return _OPS[self.op](version, self.version)

    def __repr__(self):
        return "%s%s" % (self.op, self.version)


class Constraint(object):
    """All comparators must match."""

    def __init__(self, comparators, text=""):
        self.comparators = list(comparators)
        self.text = text

    def matches(self, version):
        if isinstance(version, str):
            version = Version.parse(version)
        return all(c.matches(version) for c in self.comparators)

    def __repr__(self):
        return "Constraint(%r)" % (self.text,)


def _caret(version):
    upper = Version(version.major + 1, 0, 0)
    return [Comparator(">=", version), Comparator("<", upper)]


def _tilde(version):
    upper = Version(version.major, version.minor + 1, 0)
    return [Comparator(">=", version), Comparator("<", upper)]


def _wildcard(text):
    match = _WILDCARD_RE.match(text)
    if not match:
        return None
    major, minor = match.groups()
    if major is None:
        return []
    if minor is None:
        major = int(major)
        return [Comparator(">=", Version(major, 0, 0)), Comparator("<", Version(major + 1, 0, 0))]
    major, minor = int(major), int(minor)
    return [Comparator(">=", Version(major, minor, 0)),
            Comparator("<", Version(major, minor + 1, 0))]


def parse_comparator(text):
    """The list of Comparators one constraint item stands for."""
    text = text.strip()
    wildcard = _wildcard(text)
    if wildcard is not None:
        return wildcard
    match = _COMPARATOR_RE.match(text)
    if not match:
        raise ConstraintError("bad comparator %r" % (text,))
    op, rest = match.groups()
    try:
        version = Version.parse(rest)
    except VersionError as exc:
        raise ConstraintError(str(exc))
    if op == "^":
        return _caret(version)
    if op == "~":
        return _tilde(version)
    return [Comparator(op or "=", version)]


def parse_constraint(text):
    """Parse a constraint such as '>=1.2.0, <2.0.0' or '^1.4.2'."""
    if not text or not text.strip():
        raise ConstraintError("empty constraint")
    comparators = []
    for item in text.split(","):
        if not item.strip():
            raise ConstraintError("empty item in %r" % (text,))
        comparators.extend(parse_comparator(item))
    return Constraint(comparators, text.strip())
