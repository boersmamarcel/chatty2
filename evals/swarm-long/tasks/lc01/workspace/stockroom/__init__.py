"""stockroom: a small warehouse inventory toolkit.

The package keeps a ledger of stock movements (receipts, picks, adjustments)
per SKU and lot, values the stock, allocates picks to lots first-expiry-first-out
(FEFO), suggests replenishment orders and prints plain-text reports.

Modules:

    money       Decimal helpers: parsing, rounding, formatting amounts
    units       units of measure (each, box, case) and conversion to each
    catalog     the item master (SKU records) and its CSV loader
    locations   warehouse bin codes, sorting and the picker's walking order
    lots        lot/batch records with expiry dates
    ledger      the stock movement ledger and on-hand queries
    allocation  FEFO allocation of a pick quantity across lots
    valuation   weighted-average and FIFO cost books
    reorder     replenishment suggestions
    io_csv      CSV import of movements
    reports     plain-text reports
    cli         the `python3 -m stockroom` command line
"""

__version__ = "0.9.2"
