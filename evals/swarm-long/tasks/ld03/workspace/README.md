# Outbound shipments: data dictionary

Parcel shipments from four distribution centres (DCs), shipped from late June
to the end of September 2026; the export was taken at 2026-10-01T00:00:00Z.
Comma-separated, header row.

## Files

### shipments.csv
One row per shipment **update**: the tracking feed re-sends a shipment
whenever something about it changes (status, delivery scan, corrected weight).

| column | meaning |
| -- | -- |
| shipment_id | shipment identifier |
| dc_id | shipping DC, see dcs.csv |
| carrier_code | see carriers.csv |
| dest_country | destination country code |
| shipped_at_utc | when the parcel was handed over at the DC dock, UTC |
| delivered_at_utc | delivery scan, UTC; empty while not delivered |
| status | `in_transit`, `delivered`, `returned` (delivered back to the DC; not a delivery), `cancelled` (label voided, never billed) |
| weight_kg | actual weight |
| length_cm, width_cm, height_cm | parcel dimensions |
| updated_at | when this update was recorded, UTC |

### dcs.csv
`utc_offset_hours`: the DC's fixed offset from UTC for this period (local =
UTC + offset). `cutoff_local`: the daily dock cut-off in DC-local time.

### dc_holidays.csv
Days on which a DC and its carriers do not work (per DC).

### carriers.csv
`sla_business_days`: the promised transit time.

### rate_card.csv
Price per parcel in EUR by carrier and billable-weight band; bands are
inclusive on both ends and in whole kilograms.

## Business definitions

1. **Current state.** When a `shipment_id` appears more than once, only the
   row with the latest `updated_at` counts.
2. **Business day** of a DC: Monday to Friday, except that DC's holidays.
3. **Ship date.** The DC-local date of `shipped_at_utc`. If the local time is
   at or after the DC's cut-off, or that date is not a business day of the
   DC, the ship date is the next business day of the DC. Every month or
   quarter filter on shipments uses the ship date.
4. **Delivery date.** The DC-local date of `delivered_at_utc` (the DC's
   offset is used for the destination too).
5. **Transit days** = the number of business days of the DC after the ship
   date up to and including the delivery date (delivered on the ship date =
   0; a Saturday delivery counts like the Friday before).
6. **On time**: transit days <= the carrier's `sla_business_days`. On-time
   rates are over shipments whose current status is `delivered`.
7. **Billable weight** = the greater of `weight_kg` and the volumetric weight
   length x width x height / 5000 (cm, giving kg), rounded **up** to a whole
   kilogram (minimum 1 kg). The price is the carrier's band containing the
   billable weight.
8. **Freight cost** is charged for every shipment except `cancelled` ones.

Round only final answers.
