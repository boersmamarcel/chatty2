# Hotel revenue data mart: data dictionary and reporting rules

This folder holds the reservation extract of a group of five city hotels in
the Netherlands, Belgium and the United Kingdom, taken on the morning of
**1 July 2026**. It covers every booking with an arrival date from
25 February 2026 to 30 June 2026, and it is the input for the revenue
management report of the second quarter. The document has five parts: the
files (A), the reporting rules (B), worked examples and frequent questions
(C), the changelog (D) and data-quality notes with a glossary (E).

> **Precedence.** The changelog in part D amends the rules of part B. Where
> an amendment disagrees with part B, the amendment governs from its stated
> effective date (and only from that date, unless it says otherwise).
> Footnotes are part of the rules.

All files are comma-separated, UTF-8, with one header row. Dates are ISO
`YYYY-MM-DD`; timestamps are ISO 8601 UTC with a trailing `Z`. Amounts have
two decimals and no thousands separators.

---

## Part A. Files

### A.1 bookings.csv

One row per **version** of a booking. The property management system
writes a new version whenever a booking changes: a rate change, a room
change, a cancellation, the check-out, or a no-show being posted on the
morning after the arrival date. All versions are in the extract, in no
particular order.

| column | meaning |
| -- | -- |
| booking_id | booking identifier, stable across versions |
| hotel_id | the hotel, see hotels.csv |
| room_type_code | the hotel's own room type code; see room_type_map.csv for its category |
| rate_plan | the rate plan sold, see rate_plans.csv |
| arrival_date | first night of the stay |
| departure_date | the morning the guest leaves; this date is **not** a night of the stay |
| rooms | number of identical rooms in the booking |
| nightly_rate | the price of one room for one night, in the hotel's currency, **including VAT** and including breakfast where the plan includes it |
| status | `confirmed` (booked, not yet departed), `checked_out`, `cancelled`, `no_show` |
| cancelled_at | when the booking was cancelled, UTC; empty otherwise |
| modified_at | when this version was written, UTC |

### A.2 hotels.csv

| column | meaning |
| -- | -- |
| hotel_id | H1 to H5 |
| name | hotel name |
| country | NL, BE or GB; decides the VAT rate |
| currency | the currency of all rates of the hotel (EUR or GBP) |
| rooms_total | the number of rooms the hotel can sell **today** (see part D) |

### A.3 room_type_map.csv

Maps a hotel's room type code to a **room category** (`standard`, `deluxe`,
`suite`) for group reporting. The mapping is effective-dated: two room types
were re-classified in spring 2026 after renovations. A row is valid from
`valid_from` to `valid_to`, both inclusive; an empty `valid_to` is
open-ended.

### A.4 rate_plans.csv

| column | meaning |
| -- | -- |
| rate_plan | code as in bookings.csv |
| description | readable name |
| refundable | 1 = the guest may cancel free of charge; 0 = non-refundable |
| breakfast_included | 1 = the nightly rate includes breakfast |
| breakfast_value_per_room_night | the value of the breakfast component per room per night, in the hotel's currency, **excluding VAT** |
| commission_pct | commission owed to the selling channel, in percent (see B.7) |

### A.5 vat_rates.csv

Accommodation VAT per country, effective-dated (`valid_from` to `valid_to`
inclusive, empty `valid_to` = open-ended). The rate that applies to a night
is the rate valid **on that night's date**, not on the booking date.

### A.6 fx_rates.csv

Monthly rates: `eur_per_unit` = euros per one unit of `currency` in `month`
(`YYYY-MM`). EUR rows are 1.

---

## Part B. Reporting rules

### B.1 Current version

Only the latest version of each booking counts: the row with the greatest
`modified_at` for the `booking_id`. Earlier versions are history.

### B.2 Sold bookings

A booking is **sold** when its current status is `confirmed` or
`checked_out`. Cancelled bookings are not sold. No-show bookings are not
sold either[^noshow]. Bookings on the rate plan `HOLD` are allotment blocks
held for tour groups, not sales: they are excluded from every figure,
whatever their status.

### B.3 Stay nights

The nights of a booking are the dates from `arrival_date` up to and
including the day before `departure_date`. Every figure is reported by
**night date**: a stay from 30 April to 2 May has one night in April and one
in May. A booking of `rooms` = 2 produces two **room-nights** per night.

### B.4 Net room revenue

For each sold room-night:

1. take the `nightly_rate`;
2. remove VAT: divide by (1 + VAT% / 100), using the VAT rate of the hotel's
   country valid on the night's date;
3. if the rate plan includes breakfast, subtract
   `breakfast_value_per_room_night` (already excluding VAT);
4. convert to euros at the rate of the **month of the night**.

The sum over room-nights is the net room revenue. Breakfast is food and
beverage revenue, not room revenue.

### B.5 Available room-nights

The available room-nights of a hotel for a period = its rooms available per
night x the number of nights in the period. Rooms available per night =
`rooms_total`, except where part D says otherwise.

### B.6 Key ratios

- **Occupancy** = sold room-nights / available room-nights, as a percentage.
- **ADR** (average daily rate) = net room revenue in EUR / sold room-nights.
- **RevPAR** = net room revenue in EUR / available room-nights.

### B.7 Commission

Commission of a sold room-night = `commission_pct` / 100 x the net room
revenue of that room-night (after VAT and breakfast, in EUR). Plans with a
commission of 0 owe nothing.

### B.8 Room category

The category of a room-night is found through the booking's `hotel_id` and
`room_type_code` in room_type_map.csv, using the row valid on the **night's
date**.

[^noshow]: Until amendment A2 (part D). Hotels charge non-refundable
no-shows; read A2 before excluding them.

---

## Part C. Worked examples and frequent questions

**Example 1 (nights and months).** A booking from 2026-04-29 to 2026-05-02
with `rooms` = 2 has nights 29 and 30 April and 1 May: four room-nights in
April, two in May. Its April revenue uses April's VAT and April's FX rate;
its May night uses May's.

**Example 2 (net revenue).** An Amsterdam BB booking with a nightly rate of
EUR 163.50 on a night with 9% VAT: 163.50 / 1.09 = 150.00, minus breakfast
18.00 = 132.00 net room revenue per room-night.

**Example 3 (re-classification).** A London KNG room-night on 2026-04-30 is a
`standard` room-night; on 2026-05-01 it is `deluxe`. A stay across both
dates is split.

**Example 4 (versions).** A booking with versions `confirmed` (rate 120.00,
written 2026-03-01) and `confirmed` (rate 135.00, written 2026-03-05) counts
once, at 135.00.

**Q: Does a cancelled booking's cancellation fee count?** No. The extract
has no fees; cancelled bookings are excluded.

**Q: Are in-house guests (status `confirmed` with a departure in July)
sold?** Yes. Their June nights count in June; their July nights fall outside
every question in this report.

**Q: Which date decides the VAT rate, the booking date or the night?** The
night (A.5).

**Q: Is ADR computed from the gross nightly rate?** No: from net room revenue
(B.4).

**Q: A booking has `rooms` = 3 but only one guest name. Is it one
room-night per night?** No, three.

---

## Part D. Changelog (amendments to part B)

**A1 (2026-03-15). HOLD plan.** Allotment holds were moved to their own rate
plan `HOLD` (rule B.2). There are no allotment holds on other plans.

**A2 (2026-05-01). Non-refundable no-shows.** For bookings with an arrival
date **on or after 1 May 2026**, a no-show on a non-refundable rate plan
(`refundable` = 0) is charged one night. Such a booking counts as sold for
its **first night only** (its arrival date): `rooms` room-nights, with net
room revenue, category and commission computed as for any sold room-night
of that date. No-shows on refundable plans, and all no-shows with an arrival
before 1 May 2026, remain excluded.

**A3 (2026-05-01). Rotterdam inventory.** On 1 May 2026 three rooms of H2 were
converted to staff housing. `rooms_total` in hotels.csv shows the new
inventory of 42 rooms; for nights **before** 1 May 2026, H2 had **45**
rooms available per night.

**A4 (2026-06-01). Commission base.** From the night of 1 June 2026 onwards,
commission is calculated on the **gross** nightly rate (including VAT and
breakfast), converted to euros at the night's month rate, instead of on net
room revenue. Nights before 1 June keep the rule of B.7.

**A5 (2026-06-15). Clarification.** Room-type re-classifications in
room_type_map.csv apply by night date (B.8); the mapping row valid on the
booking date is irrelevant.

---

## Part E. Data-quality notes and glossary

### E.1 Known data-quality issues

1. **Version noise.** About four in ten bookings have an earlier version,
   typically with a different rate or room type (upgrades, re-pricing) or a
   `confirmed` status that was later cancelled. Use only the current version.
2. **Multi-room bookings.** About one booking in eight has more than one
   room. Counting bookings or nights instead of room-nights understates
   occupancy and inflates ADR.
3. **Month-crossing stays.** Stays that cross a month end are common in the
   extract; split them by night.
4. **GBP hotels.** Rates of H4 and H5 are in GBP, and so are their breakfast
   values. Convert after removing VAT and breakfast.
5. **Status timing.** A no-show is posted on the morning after arrival; its
   `modified_at` can be later than the arrival date.

### E.2 Glossary

- **Room-night.** One room sold for one night.
- **Sold room-night.** A room-night of a sold booking (B.2, as amended by
  A2), outside the HOLD plan.
- **Net room revenue.** B.4.
- **Occupancy, ADR, RevPAR.** B.6.
- **Night date.** The calendar date of the evening the room is occupied.

### E.3 Control totals

Front-office control totals (gross rates x rooms over all versions) are used
to reconcile the extract with the property systems. They are not reporting
figures and differ from every figure defined above.

---

## Part F. Background for analysts

### F.1 How the hotels sell

The group sells rooms through five kinds of channel. Direct bookings on the
group website use the plans `BAR` (flexible, room only), `BB` (flexible,
with breakfast) and `NRF` (non-refundable, room only, about ten percent
cheaper). Online travel agencies sell the plans `OTA` and `OTA-NRF` and keep
a commission of fifteen percent. Corporate clients book the negotiated plan
`CORP`, which always includes breakfast. Tour operators buy the package plan
`PKG`, which includes breakfast, is non-refundable and carries a ten percent
commission. Allotment holds (`HOLD`) are rooms blocked for tour groups that
the operator may or may not fill; they are not sales.

### F.2 Why breakfast is carved out

Room revenue is benchmarked against competitor hotels that sell room only.
The breakfast component of a plan that includes breakfast is therefore moved
to food and beverage revenue at a fixed value per room per night, set in
rate_plans.csv. The value is the same every night and does not depend on
the number of guests in the room.

### F.3 Why the VAT changes matter

The Dutch accommodation VAT rate rose from 9% to 21% on 1 May 2026. Rates
loaded in the reservation system include VAT, and most Dutch rates were not
re-priced, so the same gross rate yields less net room revenue from May.
Belgian (6%) and British (20%) rates did not change in the period.

### F.4 Inventory and renovations

H1 converted its executive rooms (`EXEC`) to junior suites; they are sold
under the same code and count as suites from 15 April 2026. H4 refurbished
its king rooms (`KNG`), which count as deluxe from 1 May 2026. H2 lost rooms
to staff housing on 1 May 2026 (A3). The other hotels kept their inventory.

### F.5 Reading the questions

Every question names its period in night dates. When a question asks for a
ratio, compute the numerator and the denominator over the same hotels and
nights and divide once; do not average daily or monthly ratios. Amounts in
euros are rounded to cents only in the final answer; percentages to two
decimals.

### F.6 Checklist

Before reporting a figure, check that you have: kept only the current
version of each booking; dropped cancelled bookings, HOLD bookings and the
no-shows that A2 does not cover; expanded bookings into nights and rooms;
used the VAT rate, the room category and the FX rate of the night; removed
breakfast where the plan includes it; and applied A3 and A4 where their
dates apply.

---

## Part G. A complete walk-through

Take a fictitious London booking BK0000001 at H4 with three versions:

1. written 2026-04-02, status `confirmed`, room type `STD`, plan `BB`,
   arrival 2026-04-29, departure 2026-05-02, `rooms` = 2, nightly rate
   GBP 180.00;
2. written 2026-04-20, status `confirmed`, room type `KNG`, same dates and
   plan, nightly rate GBP 204.00 (an upgrade);
3. written 2026-05-02, status `checked_out`, room type `KNG`, nightly rate
   GBP 204.00.

Only version 3 counts (B.1). The booking is sold (B.2). Its nights are
29 April, 30 April and 1 May (B.3); with two rooms that is six room-nights:
four in April and two in May.

For each room-night the net room revenue is 204.00 / 1.20 = 170.00 (British
VAT of 20%), minus the breakfast value of the BB plan, GBP 18.00, giving
GBP 152.00. The April room-nights convert at April's GBP rate and the May
room-nights at May's GBP rate (B.4).

The room category is `standard` for the April nights and `deluxe` for the
1 May night (room_type_map.csv, B.8). The BB plan has no commission.

Had the booking been a non-refundable `NRF` booking with an arrival on
29 April and status `no_show`, it would contribute nothing: amendment A2 only
covers arrivals on or after 1 May. With an arrival on 3 May it would
contribute two room-nights (two rooms, first night only) on 3 May.

Had the booking been an `OTA` booking, its commission would be 15% of the
net room revenue in euros for the April and May nights (B.7); for nights
from 1 June, 15% of the gross nightly rate in euros (A4).

The available room-nights of H4 for a period are 70 x the number of nights;
for H2 they are 45 per night before 1 May and 42 per night from 1 May (A3).

### G.1 Common mistakes seen in earlier reports

- Counting bookings instead of room-nights, or ignoring `rooms`.
- Attributing the whole stay to the arrival month.
- Using the VAT rate valid on the booking date instead of the night.
- Leaving breakfast in room revenue.
- Converting GBP with one rate for the whole quarter.
- Using today's room category for nights before a re-classification.
- Keeping HOLD blocks, which inflates occupancy.
- Using `rooms_total` of H2 for nights in April.
