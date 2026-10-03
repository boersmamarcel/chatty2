Q1: 862
Q2: 224432.57
Q3: EMEA
Q4: 7.63
Q5: C0061
Q6: 156
Q7: 73.03
Q8: S01, S03, S04, S05, S07

## Working notes

Reference answers computed by evals/swarm-long/gen_data.py from the CSV files with the README rules applied.

- question 1 - count Feb orders: needs dedup by latest ingested_at, status case, test customers, store-local date
- question 2 - Q1 net EUR: FX divide by units_per_eur per order month, refunds attributed to order, local dates
- question 3 - March net by region: FX trap (JPY/GBP volumes), refunds netted
- question 4 - app refund rate: refunds of cancelled orders ignored, refunds after March still count
- question 5 - top customer: a heavy test account (C0006) must be excluded
- question 6 - fully refunded: sums multiple partial refunds per order vs current gross
- question 7 - S06 median: JPY conversion per month and UTC+9 date boundary
- question 8 - store growth set: per-store net EUR Feb vs Mar, local month boundaries
