# Mobile billing data mart: data dictionary and rating rules

This folder holds a rating extract of a small Dutch mobile virtual network
operator (MVNO): every usage record of its subscribers with an event start
from the evening of 31 March 2026 to the end of 30 June 2026 (UTC), the
subscriber and plan master data, and the roaming tables. It is used to
re-rate the second-quarter invoices independently of the billing system.
The document has five parts: the files (A), the rating rules (B), worked
examples and frequent questions (C), the changelog (D) and data-quality notes
with a glossary (E).

> **Precedence.** The changelog in part D amends the rules of part B. Where
> an amendment disagrees with part B, the amendment governs from its stated
> effective date, and only from that date unless it says otherwise.
> Footnotes are part of the rules.

All files are comma-separated, UTF-8, with one header row. Dates are ISO
`YYYY-MM-DD`; timestamps are ISO 8601 UTC with a trailing `Z`. Monetary
amounts are in euros.

---

## Part A. Files

### A.1 usage.csv

One row per **version** of a usage record. The mediation platform re-sends a
record when it corrects it (for example a call duration fixed after a
switch restart); the re-sent row keeps the `record_id` and has a later
`ingested_at`. All versions are in the extract, in no particular order.

| column | meaning |
| -- | -- |
| record_id | usage record identifier, stable across versions |
| msisdn | the subscriber's phone number, see subscribers.csv |
| event_start_utc | when the call, message or data session started, UTC |
| event_type | `voice`, `sms` or `data` |
| quantity | voice: call duration in **seconds**; sms: number of messages (a long message counts as several); data: volume in **kilobytes** (KB) |
| country_code | the country of the network the subscriber used (ISO 3166 alpha-2) |
| ingested_at | when this version was loaded, UTC |

### A.2 subscribers.csv

| column | meaning |
| -- | -- |
| msisdn | phone number |
| account_id | billing account (several numbers may share one) |
| segment | `consumer` or `business` |
| activation_date | first day of service |
| deactivation_date | last day of service; empty while active |
| is_test | 1 = a network test SIM of the operator |

### A.3 subscriber_plans.csv

The **plan history** of each number: which plan applied from `valid_from` to
`valid_to` (both inclusive; empty `valid_to` = still valid). A number that
changed plan has two rows that do not overlap.

### A.4 plans.csv

| column | meaning |
| -- | -- |
| plan_id | plan code |
| name | plan name |
| monthly_fee_eur | the monthly subscription fee |
| incl_voice_min, incl_sms, incl_data_mb | the monthly allowances in minutes, messages and megabytes; **-1 means unlimited**, 0 means none |
| voice_eur_per_min, sms_eur_each, data_eur_per_mb | out-of-allowance (overage) rates |

### A.5 roaming_zones.csv

Maps the network country to a **zone**: `HOME` (the Netherlands), `EU`
(regulated roaming, "roam like at home") or `ROW` (rest of world). The
mapping is effective-dated (`valid_from` to `valid_to`, inclusive; empty =
open-ended); the row valid on the **event's local date** applies.

### A.6 roaming_rates.csv

Prices for usage in zone `ROW`: per minute, per message and per megabyte.

---

## Part B. Rating rules

### B.1 Current version

Only the latest version of each usage record counts: the row with the
greatest `ingested_at` for the `record_id`.

### B.2 Billable records

1. Records of test SIMs (`is_test` = 1) are never billed and are excluded
   from every figure, as are the test SIMs themselves.
2. A record is billable only when its local event date lies within the
   subscriber's service period: on or after `activation_date` and, if there
   is a deactivation date, on or before it. Records outside that period are
   network noise and are dropped.

### B.3 Local time and billing month

All rating is done in the operator's local time. For the whole extract the
local time is **UTC + 2 hours** (Central European Summer Time). The **billing
month** of a record is the month of its local event start; the local event
date decides the zone (A.5), the call rounding (B.5) and the service period
(B.2).

### B.4 Plan of a billing month

A subscriber is billed for a month when the service period overlaps that
month. The plan that applies to the whole billing month (fee, allowances and
overage rates) is the plan valid on the **last day of the month**, or on the
deactivation date if the subscriber was deactivated earlier in the month.
Plan changes inside a month are therefore not prorated.

### B.5 Voice

Each call is rounded **up** to whole minutes (a call of 61 seconds is 2
minutes; a call of 0 seconds is 0 minutes)[^round]. HOME and EU minutes of a
billing month are added up per subscriber; ROW minutes are kept apart.

### B.6 Messages

Messages are counted by `quantity`. HOME and EU messages are added up per
subscriber and month; ROW messages are kept apart.

### B.7 Data

Per subscriber and billing month, the HOME and EU kilobytes are added up and
converted to megabytes by dividing by 1024 and rounding **up** to a whole
megabyte. The ROW kilobytes are added up and rounded up the same way,
separately. Individual sessions are not rounded.

### B.8 Allowances and overage

HOME and EU usage consumes the plan's allowances. For each of minutes,
messages and megabytes, the **overage** quantity = usage minus allowance, if
positive; an unlimited allowance (-1) never has overage. The **overage
charge** = the overage quantity x the plan's overage rate, summed over the
three. ROW usage does not consume allowances.

### B.9 Roaming charge

The **roaming charge** of a billing month = ROW minutes x ROW voice rate +
ROW messages x ROW message rate + ROW megabytes x ROW data rate (A.6).

### B.10 Monthly fee and invoice

Every subscriber billed for a month pays the full `monthly_fee_eur` of the
plan of that month (B.4), even when activated or deactivated during the
month. The **invoice total** of a subscriber and month = monthly fee +
overage charge + roaming charge.

[^round]: Per-minute rounding is the rule for calls before the change in
part D (A2). Check the call's local date.

---

## Part C. Worked examples and frequent questions

**Example 1 (rounding).** A call of 85 seconds on 28 April (local) is 2
minutes. The same call on 3 May (local) is billed under A2 as 1.5 minutes.
A 30-second call on 3 May is 0.5 minutes; a 31-second call is 1 minute.

**Example 2 (local month).** A data session starting at 2026-04-30T22:30:00Z
starts at 00:30 local on 1 May: it belongs to May.

**Example 3 (zones).** A call made in the United Kingdom on 25 April is EU
usage and consumes the allowance. The same call on 5 May is ROW usage and is
charged at the roaming rates (A3).

**Example 4 (plan).** A subscriber on P-S until 17 May and on P-M from 18 May
is billed for May entirely as a P-M subscriber: P-M's fee, allowances and
overage rates.

**Example 5 (data).** Three sessions of 700 KB each in a month: 2,100 KB /
1024 = 2.05 MB, rounded up to 3 MB.

**Q: Do 0-second calls count as calls?** They are records but bill 0 minutes.

**Q: Does EU roaming cost extra?** No. EU usage is rated exactly like HOME
usage ("roam like at home").

**Q: A deactivated subscriber still has records after the deactivation
date. Are they billed?** No (B.2 rule 2). The subscriber is still billed for
the month of deactivation.

**Q: How do I count subscribers who "exceeded" an allowance?** A subscriber
exceeded an allowance in a month when the overage quantity of that allowance
is greater than zero.

**Q: Is the business discount applied to overage?** No (A4): only to the
monthly fee.

---

## Part D. Changelog (amendments to part B)

**A1 (2026-03-20). Test SIMs.** Test SIMs are flagged in subscribers.csv
(`is_test`), replacing the old number-range convention.

**A2 (2026-05-01). Per-half-minute billing.** For calls whose local event
date is **on or after 1 May 2026**, rule B.5 is replaced: each call is
rounded **up** to the next whole **30 seconds** and billed in half minutes
(billed minutes = ceil(seconds / 30) / 2). Calls before 1 May keep per-minute
rounding.

**A3 (2026-05-01). United Kingdom.** From 1 May 2026 the United Kingdom is
zone `ROW` (see roaming_zones.csv). Usage there before 1 May remains EU
usage.

**A4 (2026-06-01). Business discount.** From the June 2026 billing month
onwards, subscribers of the `business` segment get a **20% discount on the
monthly fee** (fee x 0.8). Overage and roaming charges are not discounted.
Earlier months are not restated.

**A5 (2026-06-10). Clarification of B.4.** The plan on the last day of the
month (or the deactivation date) also decides the overage rates of that
month, not only the fee.

---

## Part E. Data-quality notes and glossary

### E.1 Known data-quality issues

1. **Re-sent records.** About one record in sixteen has been re-sent, often
   with a corrected quantity. Summing all rows double counts them and uses
   wrong durations.
2. **Boundary records.** The extract starts at 20:00 UTC on 31 March so that
   it covers local 1 April from midnight. Records before local 1 April are
   outside every billing month of this report.
3. **New and leaving subscribers.** Some numbers were activated during the
   quarter, some deactivated; their SIMs can still produce records outside
   the service period.
4. **Plan changes.** About a quarter of the numbers changed plan during the
   quarter. Use subscriber_plans.csv with B.4; there is no plan column on
   the usage records.
5. **Unlimited allowances.** `-1` is a sentinel, not a number of minutes.

### E.2 Glossary

- **Billing month.** B.3.
- **Overage.** Usage above an allowance (B.8).
- **ROW.** Rest of world (zone outside HOME and EU).
- **Invoice total.** B.10.
- **MSISDN.** The subscriber's phone number.

---

## Part F. Background for analysts

### F.1 The plans

The operator sells five plans. `P-S` (Start) is a small bundle for light
users. `P-M` (Medium) has unlimited messages. `P-L` (Large) has unlimited
minutes and messages and a large data bundle. `P-B` (Business Pro) is sold
to business customers only and has the largest data bundle; business
customers may also take any other plan. `P-D` (Data only) is meant for
tablets and routers: it has no voice or message allowance at all, so every
minute and message on it is overage. Allowances are monthly and do not roll
over to the next month.

### F.2 Roaming

Inside the European Union, regulated roaming means usage abroad is rated
exactly as at home: it consumes the allowance and is charged overage rates
only when the allowance is used up. Outside the EU, usage is charged at the
ROW rates and never consumes the allowance. The United Kingdom stayed in the
EU zone under a transition arrangement that ended on 30 April 2026 (A3).
Switzerland, the United States and Turkey are ROW.

### F.3 Why records are re-sent

The mediation platform receives call detail records from the host network.
When a switch restarts, records of calls in progress can be cut short or
duplicated; the platform re-sends the corrected record later under the same
identifier. Only the latest version reflects the call as it happened.

### F.4 Reading the questions

Every question names a billing month (B.3). A question about "subscribers"
counts subscriber numbers (MSISDNs) billed for that month, not accounts.
When a question asks for an average over subscribers, divide the total by
the number of subscribers billed for the month in the group, including those
with no usage at all. Amounts are rounded to cents only in the final answer.

### F.5 Checklist

Before reporting a figure, check that you have: kept only the latest
version of each record; dropped test SIMs and records outside the service
period; moved every timestamp to local time; found the zone valid on the
local event date; rounded each call under the rule of its local date;
rounded data per subscriber and month, not per session; looked up the plan
valid on the last day of the month (or the deactivation date); treated -1
as unlimited; and applied the business discount only to June fees.

### F.6 Units

Seconds for calls in usage.csv, minutes for allowances and rates; kilobytes
for data in usage.csv, megabytes (1 MB = 1024 KB) for allowances and rates.
Messages are counted, not characters.

---

## Part G. A complete walk-through

Take a fictitious consumer subscriber 31600000001, on plan `P-S` until
17 May 2026 and on `P-M` from 18 May, and its May 2026 records:

1. a 61-second call at 2026-04-30T21:50:00Z (local 23:50 on 30 April): it
   belongs to April, not May (B.3), and is rated per minute (2 minutes);
2. a 61-second call at 2026-05-02T08:00:00Z, in the Netherlands: local date
   2 May, so A2 applies: ceil(61 / 30) / 2 = 1.5 minutes, HOME;
3. a call of 200 seconds in Germany on 10 May: ceil(200 / 30) / 2 = 3.5
   minutes, EU, so it consumes the allowance like a home call;
4. a call of 200 seconds in the United Kingdom on 12 May: 3.5 minutes, ROW
   (A3), charged at the ROW voice rate and not counted against the allowance;
5. data sessions of 600,000 KB at home and 500,000 KB in Germany: HOME+EU
   data of 1,100,000 KB / 1024 = 1,074.2 MB, rounded up to 1,075 MB;
6. a data session of 3,000 KB in the United States: ROW data, 3,000 / 1024 =
   2.93 MB, rounded up to 3 MB at the ROW data rate;
7. a record of the same call as record 2, re-sent later with a duration of
   59 seconds: only the re-sent version counts, so record 2 bills
   ceil(59 / 30) / 2 = 1.0 minute instead of 1.5.

The plan of the May billing month is `P-M`, the plan valid on 31 May (B.4):
its fee of EUR 20.00, its allowances (300 minutes, unlimited messages,
8,192 MB) and its overage rates apply to the whole month. The subscriber's
HOME+EU minutes (1.0 + 3.5 + any other calls) are compared with 300 minutes;
its 1,075 MB with 8,192 MB. Its ROW charge is 3.5 x the ROW voice rate plus
3 x the ROW data rate. The invoice total is fee + overage + roaming (B.10).

If the subscriber were in the `business` segment, its June fee would be
P-M's fee x 0.8 (A4); its May fee would not be discounted.

### G.1 Common mistakes seen in earlier re-ratings

- Using UTC dates for the billing month or for the A2/A3 switch dates.
- Treating -1 as a negative allowance (which makes every unit overage) or
  as zero.
- Rounding data per session instead of per subscriber and month.
- Treating United Kingdom usage in May as EU usage.
- Taking the plan valid on the first day of the month.
- Discounting overage or roaming for business customers.
- Billing records of test SIMs or records after deactivation.
