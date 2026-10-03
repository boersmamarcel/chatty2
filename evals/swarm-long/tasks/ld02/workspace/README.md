# SaaS billing: data dictionary

Billing data of a B2B SaaS product, January 2025 export extended to June
2026 (some annual invoices start in 2024). Comma-separated, header row.

## Files

### invoices.csv
One row per invoice **version**: the billing system re-sends an invoice when
its status changes.

| column | meaning |
| -- | -- |
| invoice_id | invoice identifier |
| account_id | customer account, see accounts.csv |
| plan_code | plan, see plans.csv |
| period_start | first day of the service period the invoice pays for |
| amount | invoice total in `currency`, before credit notes |
| currency | USD, EUR or GBP |
| status | `draft`, `open` (issued, not yet paid), `paid` or `void` (cancelled; a corrected invoice may be issued under a new id) |
| ingested_at | when this version was exported, UTC |

### plans.csv
`plan_code`, `tier` (starter, team, business), `billing_period` (`monthly`:
the invoice covers one month; `annual`: it covers twelve months starting at
`period_start`) and the list price (informational only; invoices carry the
actual amount).

### credit_notes.csv
Credits against an invoice, in the invoice's currency. An invoice can have
several.

### accounts.csv
`account_id`, `name`, `segment` (smb, mid_market, enterprise), `country`,
`currency`, `is_internal` (1 = the vendor's own demo/staff account),
`created_at` (when the account record was created; not a billing date).

### fx_rates.csv
`usd_per_unit`: US dollars per one unit of `currency`, per `month` (YYYY-MM).

## Business definitions

1. **Current version.** When an `invoice_id` appears more than once, only the
   row with the latest `ingested_at` counts.
2. **Billable invoice.** Current status `paid` or `open`. Draft and void
   invoices count nowhere, and neither do their credit notes. Internal
   accounts are excluded from every figure.
3. **Net invoice amount** = `amount` minus all credit notes of the invoice.
4. **USD.** Convert the net invoice amount at the rate of the month of
   `period_start` (for annual invoices too: one rate for all twelve months).
5. **MRR of a month** = the sum over billable invoices whose service period
   covers that month of: the net USD amount for monthly plans; one twelfth of
   the net USD amount for annual plans (spread evenly over the twelve months
   from `period_start`, including months before 2026 or after the data ends).
6. **Active account in month M**: its MRR in M is greater than zero.
7. **Churned in M**: active in the month before M and not active in M.
8. **New account in M**: active in M and not active in any earlier month.
   An account's tier in a month is the tier of the plan of its billable
   invoice covering that month.
9. **ARPA** of a group in M = the group's MRR in M divided by its number of
   active accounts in M.

Round only final answers.
