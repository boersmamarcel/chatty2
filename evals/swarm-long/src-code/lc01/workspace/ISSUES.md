# Open issues

Seven open issues, reported by the warehouse team. They are independent of
each other. Each lists its acceptance criteria; the behaviour described there
is what will be checked, including every edge case listed.

---

## Issue 1: FEFO allocation ships lots on their expiry day and ignores receipt order

Reported by: outbound shift lead

Last Tuesday a pick shipping on 2026-03-10 was allocated from lot L-77, which
expires on 2026-03-10. A lot cannot be shipped on its expiry date (see
`Lot.is_expired`). Also, when two lots expire on the same day, the allocator
picks by lot id, so a fresher lot that happens to have a smaller id is used
before older stock.

Acceptance (`stockroom.allocation`):

- `eligible_lots` / `allocate` never use a lot whose expiry date is on or
  before the ship date. The `min_remaining_days` rule is unchanged: a lot
  needs at least `min_remaining_days` days between the ship date and its
  expiry date.
- `fefo_order` sorts lots with an expiry date first, by expiry date
  ascending; ties are broken by received date ascending (a lot with no
  received date comes after the lots with one), then by lot id ascending.
- Lots without an expiry date come after all lots with one, ordered by
  received date ascending (no received date last), then lot id. Mixing lots
  with and without received dates must not raise.
- `allocate` still raises `AllocationError` (with `shortfall`) when the
  eligible stock is insufficient.

## Issue 2: multi-level pack sizes are read wrong; fractional boxes are truncated

Reported by: purchasing

The catalog's `pack` column uses the supplier notation `12x6`, meaning a box
holds 12 inner packs of 6 eaches, i.e. 72 eaches. `load_catalog` reads it as
12. Separately, a movement of `1.3` boxes of a 4-pack item was booked as 5
eaches: the conversion silently drops the fraction instead of rejecting a
quantity that is not a whole number of eaches.

Acceptance:

- `stockroom.catalog.parse_pack(text)` returns the product of all factors:
  `"24"` -> 24, `"12x6"` -> 72, `"2x3x4"` -> 24. The `x` is
  case-insensitive and may be surrounded by spaces (`"12 X 6"` -> 72);
  surrounding whitespace is ignored.
- `parse_pack` raises `CatalogError` for an empty value, an empty factor
  (`"x6"`, `"12x"`), a factor that is not a positive whole number (`"0"`,
  `"12x0"`, `"-3"`, `"abc"`, `"1.5"`).
- `load_catalog` therefore stores `pack_size == 72` for `pack` `12x6`.
- `stockroom.units.to_each(qty, unit, item)` returns an `int` when the
  converted quantity is a whole number of eaches (`Decimal("1.5")` boxes of a
  4-pack -> 6; `-2` boxes -> -8) and raises `UnitError` when it is not
  (`Decimal("1.3")` boxes of a 4-pack; `"0.5"` eaches).

## Issue 3: average cost drifts and stock-outs leave a residual value

Reported by: finance

The weighted average unit cost is rounded with float `round()`, so receiving
32 eaches for a total of 1.00 gives an average of 0.0312 instead of 0.0313.
After a SKU is sold out completely the stock value report still shows a few
cents (or a negative value) for it. Issuing more than is on hand is accepted
and drives the quantity negative.

Acceptance (`stockroom.valuation.AverageCostBook`):

- After every receipt `avg_cost` is `value / qty` rounded half-up to 4
  decimals (a `Decimal`).
- An `issue` that takes the entire remaining quantity returns the entire
  remaining `value` as the cost of goods issued; afterwards `qty == 0`,
  `value == Decimal("0.00")` and `avg_cost == Decimal("0")`. Other issues
  still cost `qty * avg_cost` rounded half-up to cents.
- Issuing more than `qty` raises `ValuationError` and leaves `qty`, `value`
  and `avg_cost` unchanged.

Example: receive 3000 at 0.033334 (value 100.00, average 0.0333), issue 1000
(cost 33.30), issue 2000 (cost 66.70, not 66.60); the book is then empty.

## Issue 4: replenishment quantities ignore supplier minimums and order down

Reported by: planner

`suggest_qty` rounds the needed quantity to the *nearest* order multiple, so
a need of 25 with a multiple of 10 orders 20 and we run short again. It also
ignores the supplier's minimum order quantity and keeps suggesting orders for
discontinued items.

Acceptance (`stockroom.reorder`):

- The trigger is unchanged: an order is suggested only when
  `on_hand + on_order` is at or below `item.reorder_point`, for the gap up
  to `target_level(item, daily_demand)`.
- The quantity is the gap rounded **up** to the next multiple of
  `item.order_multiple` (a multiple of 0 or 1 means no rounding).
- If that is below `item.min_order_qty`, the quantity is `min_order_qty`
  rounded up to a multiple of `order_multiple`.
- Discontinued items (`item.discontinued`) always get 0, and
  `build_suggestions` leaves them out.

Example: reorder point 50, lead time 5 days, demand 3.2/day (target 66),
position 40: gap 26 -> 30 with multiple 10; with multiple 12 and minimum
100 -> 108.

## Issue 5: bins are sorted as text; the pick path makes pickers walk back

Reported by: outbound shift lead

`sort_bins` sorts the canonical strings, so `A-100-1` comes before `A-99-1`
and aisle `AA` before aisle `B`. The pick list also sends pickers up every
aisle from rack 1, although the warehouse is walked in a serpentine.
Scanners sometimes send lower-case codes with trailing spaces, which
`parse_bin` rejects.

Acceptance (`stockroom.locations`):

- `parse_bin` accepts lower-case letters and surrounding whitespace
  (`" b-7-2 "` -> `BinCode('B', 7, 2)`); it still raises `LocationError` for
  malformed codes and for rack 0.
- `sort_bins(codes)` returns the canonical strings of the distinct bins
  (each bin once, however it was spelled), ordered by aisle number
  (`aisle_number`: A..Z, then AA, AB, ...), then rack, then level, all
  numerically.
- `pick_path(codes)` returns the distinct canonical bins in walking order:
  aisles by aisle number; in odd-numbered aisles (A, C, E, ..., AA) racks
  ascending, in even-numbered aisles (B, D, ..., Z, AB) racks descending;
  within one rack, levels always ascending.

## Issue 6: movement import rejects valid exports and double-books re-sent rows

Reported by: inventory control

The new terminals export quantities with a thousands separator (`"1,200"`)
and dates as `DD/MM/YYYY`; both rows are rejected. When a terminal re-sends
a batch, the same movement id appears twice in the file and the stock is
booked twice. Finally the line numbers in the error list do not match the
file when opened in an editor.

Acceptance (`stockroom.io_csv.read_movements`):

- `qty` may contain `,` thousands separators: `"1,200"` is 1200.
- `date` is accepted as `YYYY-MM-DD` or `DD/MM/YYYY` (`04/03/2026` is
  4 March 2026). Anything else is a row error.
- A row whose `movement_id` equals that of an earlier *accepted* row is
  skipped: it is not a movement and not an error, and its id is appended to
  `result.duplicates` (once per skipped row, in file order). A row whose id
  only matches an earlier *rejected* row is read normally.
- Every error message is `"line <n>: <reason>"` where `<n>` is the line
  number in the file: the header is line 1, and blank lines count.

## Issue 7: stock value report misaligned and hard to read

Reported by: finance

Amounts in reports have no thousands separators, negative values (stock
booked below zero) show as `-4500.00` instead of the accounting style
`(4,500.00)`, and the fixed column widths break as soon as a SKU or value
is long.

Acceptance:

- `stockroom.money.format_amount(value, places=2, accounting=False)`
  rounds half-up to `places` decimals and groups the integer part with `,`
  (`1234567.891` -> `"1,234,567.89"`). Negative amounts are `"-1,234.50"`,
  or `"(1,234.50)"` with `accounting=True`. An amount that rounds to zero is
  `"0.00"` in both modes (never `"-0.00"` or `"(0.00)"`).
- `stockroom.reports.stock_value_report(rows)` prints three columns `SKU`,
  `QTY`, `VALUE` separated by two spaces. Every column is as wide as its
  widest cell (header and TOTAL row included); `SKU` is left-aligned, `QTY`
  and `VALUE` are right-aligned. Values use `format_amount(...,
  accounting=True)`. Lines have no trailing spaces and are joined with
  `"\n"` (no final newline). Example for
  `[("AB-1", 12, Decimal("2.50")), ("LONGSKU-77", -3, Decimal("1500.00")),
  ("C", 1000, Decimal("12.345"))]`:

```
SKU          QTY       VALUE
AB-1          12       30.00
LONGSKU-77    -3  (4,500.00)
C           1000   12,345.00
TOTAL       1009    7,875.00
```
