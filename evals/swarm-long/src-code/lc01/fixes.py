"""Reference fixes for lc01 (stockroom): exact replacements per issue."""

FIXES = {
    1: [
        ("stockroom/allocation.py",
         """            if lot.expiry < ship_date:
                continue""",
         """            if lot.expiry <= ship_date:
                continue"""),
        ("stockroom/allocation.py",
         """    def key(lot):
        has_expiry = 0 if lot.expiry is not None else 1
        return (has_expiry, lot.expiry or lot.received, lot.lot_id)
    return sorted(lots, key=key)""",
         """    def key(lot):
        has_expiry = 0 if lot.expiry is not None else 1
        expiry = lot.expiry or datetime.date.min
        no_received = 0 if lot.received is not None else 1
        received = lot.received or datetime.date.min
        return (has_expiry, expiry, no_received, received, lot.lot_id)
    return sorted(lots, key=key)"""),
        ("stockroom/allocation.py",
         """import collections
""",
         """import collections
import datetime
"""),
    ],
    2: [
        ("stockroom/catalog.py",
         """    first = text.split("x")[0]
    try:
        size = int(first)
    except ValueError:
        raise CatalogError("bad pack size: %r" % (text,))
    if size <= 0:
        raise CatalogError("pack size must be positive: %r" % (text,))
    return size""",
         """    size = 1
    for factor in text.split("x"):
        factor = factor.strip()
        if not factor.isdigit():
            raise CatalogError("bad pack size: %r" % (text,))
        value = int(factor)
        if value <= 0:
            raise CatalogError("pack size must be positive: %r" % (text,))
        size *= value
    return size"""),
        ("stockroom/units.py",
         """    qty = Decimal(str(qty))
    eaches = qty * factor(unit, item)
    return int(eaches)""",
         """    qty = Decimal(str(qty).strip())
    eaches = qty * factor(unit, item)
    if eaches != eaches.to_integral_value():
        raise UnitError("%s %s of %s is not a whole number of eaches" % (qty, unit, item.sku))
    return int(eaches)"""),
    ],
    3: [
        ("stockroom/valuation.py",
         """from decimal import Decimal
""",
         """from decimal import Decimal, ROUND_HALF_UP
"""),
        ("stockroom/valuation.py",
         """        self.avg_cost = Decimal(str(round(float(self.value) / self.qty, UNIT_COST_PLACES)))
        return self.avg_cost""",
         """        self.avg_cost = (self.value / self.qty).quantize(
            Decimal(1).scaleb(-UNIT_COST_PLACES), rounding=ROUND_HALF_UP)
        return self.avg_cost"""),
        ("stockroom/valuation.py",
         """        cogs = round_money(qty * self.avg_cost)
        self.qty -= qty
        self.value -= cogs
        return cogs""",
         """        if qty > self.qty:
            raise ValuationError("%s: cannot issue %d, only %d on hand" % (self.sku, qty, self.qty))
        if qty == self.qty:
            cogs = self.value
            self.qty = 0
            self.value = Decimal("0.00")
            self.avg_cost = Decimal("0")
            return cogs
        cogs = round_money(qty * self.avg_cost)
        self.qty -= qty
        self.value -= cogs
        return cogs"""),
    ],
    4: [
        ("stockroom/reorder.py",
         """    position = on_hand + on_order
    if position > item.reorder_point:
        return 0
    need = target_level(item, daily_demand) - position
    if need <= 0:
        return 0
    multiple = item.order_multiple or 1
    return int(round(float(need) / multiple)) * multiple""",
         """    if item.discontinued:
        return 0
    position = on_hand + on_order
    if position > item.reorder_point:
        return 0
    need = target_level(item, daily_demand) - position
    if need <= 0:
        return 0
    multiple = item.order_multiple or 1

    def round_up(n):
        return -(-n // multiple) * multiple

    qty = round_up(need)
    if qty < item.min_order_qty:
        qty = round_up(item.min_order_qty)
    return qty"""),
        ("stockroom/reorder.py",
         """        item = items[sku]
        oh = on_hand.get(sku, 0)""",
         """        item = items[sku]
        if item.discontinued:
            continue
        oh = on_hand.get(sku, 0)"""),
    ],
    5: [
        ("stockroom/locations.py",
         """    m = _BIN_RE.match(text)
    if not m:""",
         """    m = _BIN_RE.match((text or "").strip().upper())
    if not m:"""),
        ("stockroom/locations.py",
         """def sort_bins(codes):
    \"\"\"Return the canonical form of ``codes`` in warehouse order.\"\"\"
    return sorted(str(parse_bin(c)) for c in codes)


def pick_path(codes):
    \"\"\"The order in which a picker walks the given bins.\"\"\"
    return sort_bins(codes)""",
         """def _distinct(codes):
    return set(parse_bin(c) for c in codes)


def sort_bins(codes):
    \"\"\"Return the canonical form of ``codes`` in warehouse order.\"\"\"
    bins = sorted(_distinct(codes), key=lambda b: (aisle_number(b.aisle), b.rack, b.level))
    return [str(b) for b in bins]


def pick_path(codes):
    \"\"\"The order in which a picker walks the given bins.\"\"\"
    def key(b):
        number = aisle_number(b.aisle)
        rack = b.rack if number % 2 == 1 else -b.rack
        return (number, rack, b.level)
    return [str(b) for b in sorted(_distinct(codes), key=key)]"""),
    ],
    6: [
        ("stockroom/io_csv.py",
         """    try:
        return datetime.datetime.strptime(text, "%Y-%m-%d").date()
    except ValueError:
        raise ValueError("bad date %r" % text)""",
         """    for fmt in ("%Y-%m-%d", "%d/%m/%Y"):
        try:
            return datetime.datetime.strptime(text, fmt).date()
        except ValueError:
            pass
    raise ValueError("bad date %r" % text)"""),
        ("stockroom/io_csv.py",
         """    text = (text or "").strip()
    if not text:
        raise ValueError("missing quantity")
    int(text)
    return text""",
         """    text = (text or "").strip().replace(",", "")
    if not text:
        raise ValueError("missing quantity")
    int(text)
    return text"""),
        ("stockroom/io_csv.py",
         """    rows = [row for row in reader if any(cell.strip() for cell in row)]
    for index, row in enumerate(rows, start=1):
        record = dict(zip(header, row))
        try:
            movement = _row_to_movement(record, catalog)
        except (ValueError, LedgerError, LotError, UnitError) as exc:
            result.errors.append("line %d: %s" % (index, exc))
            continue
        result.movements.append(movement)""",
         """    seen = set()
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
        result.movements.append(movement)"""),
    ],
    7: [
        ("stockroom/money.py",
         """def format_amount(value, places=2):
    \"\"\"Format an amount for reports: ``1234.5`` -> ``"1234.50"``.

    Rounds half-up to ``places`` decimals first.
    \"\"\"
    rounded = round_money(value, places)
    return "{0:.{1}f}".format(rounded, places)""",
         """def format_amount(value, places=2, accounting=False):
    \"\"\"Format an amount for reports: ``1234.5`` -> ``"1,234.50"``.

    Rounds half-up to ``places`` decimals first. Negative amounts are
    ``"-1,234.50"``, or ``"(1,234.50)"`` with ``accounting=True``.
    \"\"\"
    rounded = round_money(value, places)
    if rounded == 0:
        rounded = abs(rounded)
    text = "{0:,.{1}f}".format(abs(rounded), places)
    if rounded < 0:
        return "(%s)" % text if accounting else "-" + text
    return text"""),
        ("stockroom/reports.py",
         """    lines = ["%-10s%8s%12s" % ("SKU", "QTY", "VALUE")]
    total_qty = 0
    total_value = ZERO
    for sku, qty, unit_cost in rows:
        value = round_money(qty * unit_cost)
        total_qty += qty
        total_value += value
        lines.append("%-10s%8d%12s" % (sku, qty, format_amount(value)))
    lines.append("%-10s%8d%12s" % ("TOTAL", total_qty, format_amount(total_value)))
    return "\\n".join(lines)""",
         """    cells = [("SKU", "QTY", "VALUE")]
    total_qty = 0
    total_value = ZERO
    for sku, qty, unit_cost in rows:
        value = round_money(qty * unit_cost)
        total_qty += qty
        total_value += value
        cells.append((sku, str(qty), format_amount(value, accounting=True)))
    cells.append(("TOTAL", str(total_qty), format_amount(total_value, accounting=True)))
    widths = [max(len(c[i]) for c in cells) for i in range(3)]
    lines = ["%s  %s  %s" % (c[0].ljust(widths[0]), c[1].rjust(widths[1]), c[2].rjust(widths[2]))
             for c in cells]
    return "\\n".join(line.rstrip() for line in lines)"""),
    ],
}
