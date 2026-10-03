Q1: 1287
Q2: 90.10
Q3: DHX
Q4: 4755.70
Q5: 2.66
Q6: MX
Q7: 77
Q8: 7799

## Working notes

Reference answers computed by evals/swarm-long/gen_data.py from the CSV files with the README rules applied.

- question 1 - Aug delivered: latest update wins, ship date with cut-off/holiday roll and DC timezone
- question 2 - DHX on-time: 1-day SLA, business days per DC calendar incl. holidays, Saturday deliveries
- question 3 - worst carrier: per-carrier on-time over delivered Q3 shipments
- question 4 - SIN1 freight: volumetric billable weight rounded up, inclusive bands, returned/in-transit billed, cancelled not
- question 5 - ATL1 transit: US holidays (Jul 3, Sep 7), UTC-4 dates, 15:00 cut-off
- question 6 - late by country: on-time rule applied per shipment
- question 7 - stuck parcels: only the latest update counts; earlier in_transit rows of delivered parcels are noise
- question 8 - UPG billable kg: volumetric max then ceil per parcel, corrected weights from later updates
