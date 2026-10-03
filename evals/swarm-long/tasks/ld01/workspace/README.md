# Retail orders: data dictionary

Order data of eight stores for the first quarter of 2026, exported from the
order feed. All files are comma-separated with a header row.

## Files

### orders.csv
One row per order **version** (the feed re-sends an order when it changes).

| column | meaning |
| -- | -- |
| order_id | order identifier |
| customer_id | customer, see customers.csv |
| store_id | store, see stores.csv |
| order_ts_utc | when the order was placed, UTC (ISO 8601, `Z`) |
| channel | `web`, `store` or `app` |
| gross_amount | order value in the **store's** currency (see stores.csv); JPY has no decimals |
| status | `completed`, `cancelled` or `pending` (see rule 2) |
| ingested_at | when this version reached the warehouse, UTC |

### refunds.csv
Money returned to customers. `amount` is in the currency of the order's store.
An order can have several refunds.

### stores.csv
`store_id`, `city`, `region`, `currency` and `utc_offset_hours`: the store's
fixed offset from UTC for the whole period (local time = UTC + offset; no
daylight-saving changes are applied).

### customers.csv
`customer_id`, `signup_date`, `is_test` (1 = internal test account).

### fx_rates.csv
Monthly rates: `units_per_eur` = how many units of `currency` buy one euro in
that `month` (YYYY-MM). EUR rows are 1.

## Business definitions

1. **Current version.** When an `order_id` appears more than once, only the
   row with the latest `ingested_at` counts; earlier rows are superseded,
   whatever order they appear in.
2. **Valid order.** The current version has status `completed`. Status values
   are case-insensitive and may carry stray spaces. Cancelled and pending
   orders are not revenue. Orders of test customers (`is_test` = 1) are
   excluded from every figure.
3. **Order date.** The store-local calendar date of `order_ts_utc` (UTC plus
   the store's `utc_offset_hours`). Every month, quarter or date filter uses
   the order date. Q1 2026 = order dates 2026-01-01 to 2026-03-31.
4. **Euro conversion.** EUR = local amount / `units_per_eur` of the store's
   currency for the month of the order date.
5. **Refunds** belong to their order: they count in the order's month (not
   the month the refund was paid) and convert at the order's rate. Refunds of
   orders that are not valid are ignored.
6. **Net revenue** = gross amount minus refunds, of valid orders, in EUR.
7. **Fully refunded** = the order's refunds add up to at least its gross
   amount.

Round only final answers, not intermediate values.
