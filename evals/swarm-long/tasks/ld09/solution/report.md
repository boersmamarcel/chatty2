Q1: 3234.5
Q2: 1531.47
Q3: 14904.22
Q4: 34
Q5: 2334.00
Q6: 354.12
Q7: 214966
Q8: 41

## Working notes

Reference answers computed by evals/swarm-long/gen_data.py from the CSV files with the README rules applied.

- question 1 - May minutes: half-minute rounding from 05-01 (A2), UK is ROW from May (A3), local month, re-sent records, test/inactive out
- question 2 - April overage: per-minute rounding, unlimited -1, plan on last day of month, data MB rounded up per month, UK still EU
- question 3 - May roaming: zone by event date incl. UK->ROW, ROW MB rounded up separately, half-minute rounding
- question 4 - June data overage count: plan on 06-30 (or deactivation date), HOME+EU KB only, ceil per month
- question 5 - June fees: business 20% discount (A4), plan on last day, mid-month activations pay full fee, test SIMs out
- question 6 - May business invoice: fee (no discount yet) + overage + roaming over billed business subscribers
- question 7 - June overage MB: ceil(KB/1024) per subscriber-month, plan allowance by last-day rule
- question 8 - April SMS: multipart quantity, unlimited plans, data-only plan has 0 SMS allowance, UK still EU in April
