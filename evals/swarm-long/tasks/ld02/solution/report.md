Q1: 676602.66
Q2: 271
Q3: 15
Q4: 92.97
Q5: GB
Q6: 7906.31
Q7: 84
Q8: 26.50

## Working notes

Reference answers computed by evals/swarm-long/gen_data.py from the CSV files with the README rules applied.

- question 1 - June MRR: annual spread /12 incl. 2025 annual invoices, credits, FX at period_start month, void/draft/internal excluded
- question 2 - active count: annual accounts invoiced in 2025 still active; fully credited invoices make MRR 0
- question 3 - churn: active March, not April; void+reissue and re-sent status changes
- question 4 - NRR cohort: fixed January cohort, churned members count as 0 in June
- question 5 - country growth: Jan vs Jun MRR per country, internal accounts and FX matter
- question 6 - enterprise ARPA: MRR / active enterprise accounts, annual spread
- question 7 - new accounts: reactivated accounts (paused, then back) are not new; annual accounts count once
- question 8 - annual share: needs annual spread over 12 months including invoices from mid-2025
