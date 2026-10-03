"""Units of measure.

Stock is always kept in the base unit ``EA`` (each). Purchase orders and
picks may be expressed in boxes (``BX``) or cases (``CS``); the item master
says how many eaches a box holds (``Item.pack_size``) and how many boxes a
case holds (``Item.case_boxes``).
"""

from decimal import Decimal

BASE_UNIT = "EA"

_ALIASES = {
    "EA": "EA", "EACH": "EA", "PC": "EA", "PCS": "EA", "PIECE": "EA",
    "BX": "BX", "BOX": "BX", "BOXES": "BX",
    "CS": "CS", "CASE": "CS", "CASES": "CS",
}


class UnitError(ValueError):
    """Raised for unknown units or quantities that do not convert cleanly."""


def normalize_unit(code):
    """Return the canonical unit code for ``code`` (case-insensitive).

    >>> normalize_unit(" box ")
    'BX'
    """
    if code is None:
        return BASE_UNIT
    key = str(code).strip().upper()
    if not key:
        return BASE_UNIT
    try:
        return _ALIASES[key]
    except KeyError:
        raise UnitError("unknown unit of measure: %r" % (code,))


def factor(unit, item):
    """How many eaches one ``unit`` of ``item`` is."""
    unit = normalize_unit(unit)
    if unit == "EA":
        return 1
    if unit == "BX":
        if not item.pack_size:
            raise UnitError("%s has no pack size" % item.sku)
        return item.pack_size
    if unit == "CS":
        if not item.pack_size or not item.case_boxes:
            raise UnitError("%s has no case configuration" % item.sku)
        return item.pack_size * item.case_boxes
    raise UnitError("unsupported unit %r" % unit)


def to_each(qty, unit, item):
    """Convert ``qty`` of ``unit`` into a whole number of eaches.

    ``qty`` may be an int or a Decimal (``Decimal("1.5")`` boxes).
    """
    qty = Decimal(str(qty))
    eaches = qty * factor(unit, item)
    return int(eaches)


def from_each(eaches, unit, item):
    """Express ``eaches`` in ``unit`` as ``(whole_units, leftover_eaches)``.

    >>> from_each(30, "BX", item_with_pack_12)
    (2, 6)
    """
    f = factor(unit, item)
    return divmod(int(eaches), f)


def describe(eaches, item):
    """Human-readable breakdown, largest unit first: ``"2 CS 1 BX 3 EA"``."""
    eaches = int(eaches)
    if eaches == 0:
        return "0 EA"
    sign = "-" if eaches < 0 else ""
    rest = abs(eaches)
    parts = []
    for unit in ("CS", "BX"):
        try:
            f = factor(unit, item)
        except UnitError:
            continue
        if f > 1 and rest >= f:
            n, rest = divmod(rest, f)
            parts.append("%d %s" % (n, unit))
    if rest or not parts:
        parts.append("%d EA" % rest)
    return sign + " ".join(parts)
