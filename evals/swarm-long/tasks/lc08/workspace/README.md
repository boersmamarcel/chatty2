# splitbill

Share group expenses (trips, households) and settle up (Python 3, standard
library only): CSV import with currency conversion, equal / exact /
percentage / weighted splits, net balances and the transfers that settle
them.

    python3 -m splitbill.cli expenses.csv --rates rates.txt --summary

See the module docstrings in `splitbill/` for the details, and `ISSUES.md`
for the open issues.

## Tests

    python3 -m unittest discover -s tests -t .
