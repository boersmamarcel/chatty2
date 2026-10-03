"""CSV import of stock movements.

The warehouse management terminals export movements as CSV with the header::

    movement_id,date,sku,kind,qty,unit,lot,expiry,bin,note

``unit`` is optional (eaches by default); quantities in boxes or cases are
converted with the item master. Bad rows do not abort the import: they are
collected in :attr:`ImportResult.errors` and the remaining rows are read.
"""

import csv
import datetime
import io

from .ledger import LedgerError, make_movement
from .lots import LotError, parse_date
from .units import UnitError, to_each

COLUMNS = ("movement_id", "date", "sku", "kind", "qty", "unit", "lot", "expiry", "bin", "note")
REQUIRED = ("movement_id", "date", "sku", "kind", "qty")


class ImportError_(ValueError):
    """Raised when the file as a whole cannot be read (e.g. a missing column)."""


class ImportResult(object):
    """Outcome of :func:`read_movements`.

    ``movements``: the valid movements in file order.
    ``errors``: one message per rejected row, ``"line <n>: <reason>"``.
    ``duplicates``: movement ids of rows skipped as duplicates.
    """

    def __init__(self):
        self.movements = []
        self.errors = []
        self.duplicates = []

    def __repr__(self):
        return "ImportResult(%d movements, %d errors, %d duplicates)" % (
            len(self.movements), len(self.errors), len(self.duplicates))

    @property
    def ok(self):
        return not self.errors


def parse_movement_date(text):
    """The movement date of a row."""
    text = (text or "").strip()
    if not text:
        raise ValueError("missing date")
    for fmt in ("%Y-%m-%d", "%d/%m/%Y"):
        try:
            return datetime.datetime.strptime(text, fmt).date()
        except ValueError:
            pass
    raise ValueError("bad date %r" % text)


def parse_qty(text):
    """The quantity of a row as a string for :func:`~stockroom.units.to_each`."""
    text = (text or "").strip().replace(",", "")
    if not text:
        raise ValueError("missing quantity")
    int(text)
    return text


def read_movements(text, catalog=None):
    """Parse movements CSV ``text``.

    ``catalog`` (``{sku: Item}``) is needed to convert box/case quantities;
    rows in eaches do not need it.
    """
    reader = csv.reader(io.StringIO(text))
    try:
        header = [h.strip().lower() for h in next(reader)]
    except StopIteration:
        raise ImportError_("empty file")
    missing = [c for c in REQUIRED if c not in header]
    if missing:
        raise ImportError_("missing columns: %s" % ", ".join(missing))
    result = ImportResult()
    seen = set()
    for row in reader:
        if not any(cell.strip() for cell in row):
            continue
        line = reader.line_num
        record = dict(zip(header, row))
        try:
            movement = _row_to_movement(record, catalog)
        except (ValueError, LedgerError, LotError, UnitError) as exc:
            result.errors.append("line %d: %s" % (line, exc))
            continue
        if movement.movement_id in seen:
            result.duplicates.append(movement.movement_id)
            continue
        seen.add(movement.movement_id)
        result.movements.append(movement)
    return result


def _row_to_movement(record, catalog):
    movement_id = (record.get("movement_id") or "").strip()
    if not movement_id:
        raise ValueError("missing movement_id")
    sku = (record.get("sku") or "").strip().upper()
    if not sku:
        raise ValueError("missing sku")
    date = parse_movement_date(record.get("date"))
    qty_text = parse_qty(record.get("qty"))
    unit = (record.get("unit") or "").strip() or "EA"
    if unit.upper() in ("EA", "EACH"):
        qty = int(qty_text)
    else:
        if catalog is None or sku not in catalog:
            raise ValueError("unit %s needs the catalog entry of %s" % (unit, sku))
        qty = to_each(qty_text, unit, catalog[sku])
    kind = (record.get("kind") or "").strip()
    if kind.upper() == "PICK" or kind.upper() == "RECEIPT":
        qty = abs(qty)
    return make_movement(movement_id, date, sku, kind, qty,
                         lot=(record.get("lot") or "").strip() or None,
                         expiry=parse_date(record.get("expiry")),
                         bin=(record.get("bin") or "").strip() or None,
                         note=(record.get("note") or "").strip())


def write_movements(movements):
    """Serialise movements back to CSV text (eaches, picks as positive qty)."""
    buf = io.StringIO()
    writer = csv.writer(buf, lineterminator="\n")
    writer.writerow(COLUMNS)
    for m in movements:
        qty = -m.qty if m.kind == "PICK" else m.qty
        writer.writerow([m.movement_id, m.date.isoformat(), m.sku, m.kind, qty, "EA",
                         m.lot or "", m.expiry.isoformat() if m.expiry else "",
                         m.bin or "", m.note or ""])
    return buf.getvalue()
