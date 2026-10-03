"""The item master: one :class:`Item` per SKU.

The catalog is maintained by purchasing as a CSV export with the columns::

    sku,name,category,pack,case_boxes,unit_cost,reorder_point,
    min_order_qty,order_multiple,lead_time_days,status

``pack`` is the box configuration as printed on the supplier's spec sheet,
``status`` is ``active`` or ``discontinued``.
"""

import csv
import io
from decimal import Decimal

from .money import MoneyError, to_decimal


class CatalogError(ValueError):
    """Raised for malformed catalog data."""


class Item(object):
    """One SKU of the item master.

    Quantities (``reorder_point``, ``min_order_qty``, ``order_multiple``) are
    in eaches. ``pack_size`` is the number of eaches in one box.
    """

    __slots__ = ("sku", "name", "category", "pack_size", "case_boxes", "unit_cost",
                 "reorder_point", "min_order_qty", "order_multiple", "lead_time_days",
                 "discontinued")

    def __init__(self, sku, name="", category="", pack_size=1, case_boxes=0,
                 unit_cost=Decimal("0"), reorder_point=0, min_order_qty=0,
                 order_multiple=1, lead_time_days=0, discontinued=False):
        self.sku = sku
        self.name = name
        self.category = category
        self.pack_size = pack_size
        self.case_boxes = case_boxes
        self.unit_cost = unit_cost
        self.reorder_point = reorder_point
        self.min_order_qty = min_order_qty
        self.order_multiple = order_multiple
        self.lead_time_days = lead_time_days
        self.discontinued = discontinued

    def __repr__(self):
        return "Item(%r, pack_size=%r)" % (self.sku, self.pack_size)

    def __eq__(self, other):
        if not isinstance(other, Item):
            return NotImplemented
        return all(getattr(self, s) == getattr(other, s) for s in self.__slots__)

    def __ne__(self, other):
        result = self.__eq__(other)
        return result if result is NotImplemented else not result


REQUIRED_COLUMNS = ("sku", "name", "pack", "unit_cost")


def normalize_sku(text):
    """SKUs are upper-case without surrounding whitespace: ``" ab-12 "`` -> ``"AB-12"``."""
    sku = (text or "").strip().upper()
    if not sku:
        raise CatalogError("empty SKU")
    return sku


def parse_pack(text):
    """Number of eaches in one box, from the spec-sheet notation.

    >>> parse_pack("24")
    24
    """
    text = (text or "").strip().lower()
    if not text:
        raise CatalogError("empty pack size")
    size = 1
    for factor in text.split("x"):
        factor = factor.strip()
        if not factor.isdigit():
            raise CatalogError("bad pack size: %r" % (text,))
        value = int(factor)
        if value <= 0:
            raise CatalogError("pack size must be positive: %r" % (text,))
        size *= value
    return size


def _int_field(row, name, default=0):
    raw = (row.get(name) or "").strip()
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        raise CatalogError("%s: %s is not a whole number: %r" % (row.get("sku"), name, raw))


def item_from_row(row):
    """Build an :class:`Item` from one CSV row (a dict)."""
    sku = normalize_sku(row.get("sku"))
    try:
        cost = to_decimal(row.get("unit_cost") or "0")
    except MoneyError as exc:
        raise CatalogError("%s: %s" % (sku, exc))
    status = (row.get("status") or "active").strip().lower()
    if status not in ("active", "discontinued"):
        raise CatalogError("%s: unknown status %r" % (sku, status))
    return Item(
        sku=sku,
        name=(row.get("name") or "").strip(),
        category=(row.get("category") or "").strip(),
        pack_size=parse_pack(row.get("pack")),
        case_boxes=_int_field(row, "case_boxes"),
        unit_cost=cost,
        reorder_point=_int_field(row, "reorder_point"),
        min_order_qty=_int_field(row, "min_order_qty"),
        order_multiple=_int_field(row, "order_multiple", 1) or 1,
        lead_time_days=_int_field(row, "lead_time_days"),
        discontinued=(status == "discontinued"),
    )


def load_catalog(text):
    """Parse catalog CSV text into ``{sku: Item}``.

    Raises :class:`CatalogError` on a missing column or a duplicate SKU.
    """
    reader = csv.DictReader(io.StringIO(text))
    columns = [c.strip().lower() for c in (reader.fieldnames or [])]
    missing = [c for c in REQUIRED_COLUMNS if c not in columns]
    if missing:
        raise CatalogError("catalog is missing columns: %s" % ", ".join(missing))
    reader.fieldnames = columns
    items = {}
    for row in reader:
        if not any((v or "").strip() for v in row.values()):
            continue
        item = item_from_row(row)
        if item.sku in items:
            raise CatalogError("duplicate SKU %s" % item.sku)
        items[item.sku] = item
    return items


def by_category(items):
    """Group items by category: ``{category: [Item, ...]}`` sorted by SKU."""
    groups = {}
    for item in items.values():
        groups.setdefault(item.category or "uncategorised", []).append(item)
    for members in groups.values():
        members.sort(key=lambda i: i.sku)
    return groups
