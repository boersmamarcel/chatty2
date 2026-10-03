# stockroom

A small warehouse inventory toolkit (Python 3, standard library only): a stock
movement ledger per SKU and lot, FEFO allocation, average/FIFO valuation,
replenishment suggestions, CSV import and plain-text reports.

    python3 -m stockroom onhand catalog.csv movements.csv
    python3 -m stockroom value catalog.csv movements.csv costs.csv

See the module docstrings in `stockroom/` for the details, and `ISSUES.md` for
the open issues.

## Tests

    python3 -m unittest discover -s tests -t .
