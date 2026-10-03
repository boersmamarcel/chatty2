"""Warehouse bin locations.

A bin code is ``<aisle>-<rack>-<level>``: one or two letters for the aisle,
a rack number and a single-digit level, e.g. ``B-07-2``. The canonical form
pads the rack to two digits.
"""

import collections
import re

_BIN_RE = re.compile(r"^([A-Z]{1,2})-(\d{1,3})-(\d)$")


class LocationError(ValueError):
    """Raised for malformed bin codes."""


class BinCode(collections.namedtuple("BinCode", "aisle rack level")):
    """A parsed bin code."""

    __slots__ = ()

    def __str__(self):
        return "%s-%02d-%d" % (self.aisle, self.rack, self.level)


def parse_bin(text):
    """Parse a bin code.

    >>> parse_bin("B-7-2")
    BinCode(aisle='B', rack=7, level=2)
    """
    m = _BIN_RE.match((text or "").strip().upper())
    if not m:
        raise LocationError("bad bin code: %r" % (text,))
    aisle, rack, level = m.group(1), int(m.group(2)), int(m.group(3))
    if rack == 0:
        raise LocationError("rack numbers start at 1: %r" % (text,))
    return BinCode(aisle, rack, level)


def aisle_number(aisle):
    """Spreadsheet-style aisle number: A=1 ... Z=26, AA=27, AB=28."""
    n = 0
    for ch in aisle:
        n = n * 26 + (ord(ch) - ord("A") + 1)
    return n


def _distinct(codes):
    return set(parse_bin(c) for c in codes)


def sort_bins(codes):
    """Return the canonical form of ``codes`` in warehouse order."""
    bins = sorted(_distinct(codes), key=lambda b: (aisle_number(b.aisle), b.rack, b.level))
    return [str(b) for b in bins]


def pick_path(codes):
    """The order in which a picker walks the given bins."""
    def key(b):
        number = aisle_number(b.aisle)
        rack = b.rack if number % 2 == 1 else -b.rack
        return (number, rack, b.level)
    return [str(b) for b in sorted(_distinct(codes), key=key)]


def zone_of(code, zones):
    """Name of the zone containing ``code``.

    ``zones`` maps a zone name to a list of aisles, e.g.
    ``{"cold": ["A", "B"], "bulk": ["C"]}``. Returns ``None`` if no zone
    contains the aisle.
    """
    aisle = parse_bin(code).aisle
    for name in sorted(zones):
        if aisle in zones[name]:
            return name
    return None


def distance(a, b):
    """Rough walking distance in rack positions between two bins.

    Changing aisle costs the walk back to the aisle head and over (5 per
    aisle). Within an aisle it is the rack difference.
    """
    pa, pb = parse_bin(a), parse_bin(b)
    if pa.aisle == pb.aisle:
        return abs(pa.rack - pb.rack)
    return pa.rack + pb.rack + 5 * abs(aisle_number(pa.aisle) - aisle_number(pb.aisle))
