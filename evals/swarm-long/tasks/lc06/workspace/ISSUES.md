# Open issues

Seven open issues. Each one is independent of the others. Where an issue
names a function, keep its name and signature; error messages and output
formats must match exactly what is written here.

---

## Issue 1: Stop ids in the network file must be case-insensitive

Reported by the data team: a network file whose `[links]` section writes
`cen, mus, T1, 4` fails with `line 9: unknown stop 'cen'`, although the
stop is declared as `CEN` in `[stops]`. The same happens in `[transfers]`.

Stop ids are meant to be case-insensitive everywhere in the file. The
canonical form of a stop id is: surrounding whitespace removed, then upper
case (`" cen "` -> `"CEN"`). `transit.loader._norm_id` already implements
that, but only the `[stops]` section uses it.

Acceptance:

- `load_network` normalises every stop id in `[stops]`, `[links]` and
  `[transfers]` to the canonical form. The resulting `Network` only contains
  canonical ids (e.g. `net.outgoing("CEN")` lists the link written as
  `cen, mus, T1, 4`, `net.transfer_time("MUS")` returns the minutes written
  under `mus`).
- Error messages that quote a stop id quote the canonical form, e.g.
  `line 9: unknown stop 'XYZ'` for a link written with `xyz`.
- Line names are NOT stop ids: they are only stripped, never upper-cased.
  `l1` and `L1` are two different lines (`net.lines()` returns
  `['L1', 'l1']` for a file that uses both).
- Two `[stops]` rows whose ids have the same canonical form are a duplicate:
  the second one is reported as `line N: duplicate stop 'A'` (canonical id)
  and the first row is kept (name and zone of the first row).
- As before, all problems are collected and raised together in one
  `NetworkFormatError` whose `errors` list is in line order.

## Issue 2: Shortest route charges a transfer penalty for boarding

`transit.dijkstra.shortest_route(network, origin, destination,
transfer_penalty=0)` is wrong in three ways.

Acceptance:

- The cost of a route is the sum of its link minutes plus `transfer_penalty`
  for every line change. Boarding the first vehicle at the origin is not a
  change (same rule as `transit.model.count_changes`). Today a one-line trip
  of 10 minutes with `transfer_penalty=3` costs 13; it must cost 10.
- Ties between routes of equal total cost are broken deterministically, in
  this order: fewer line changes, then the lexicographically smallest list of
  stop ids, then the lexicographically smallest list of line names (this is
  `Route.key()`). The returned `Route` has `stops`, `links` and `cost` of the
  winning route.
- An unknown origin OR destination raises `UnknownStopError` naming that stop
  (today an unknown destination raises `NoRouteError`). A known but
  unreachable destination still raises `NoRouteError`, and
  `origin == destination` still returns a route with one stop, no links and
  cost 0.

## Issue 3: Earliest arrival fails for night services and tight connections

Users of the journey planner report two symptoms of
`transit.timetable.earliest_arrival`:

1. Trips that run past midnight cannot be loaded at all: `load_timetable`
   raises `TimetableError: line 7: bad time '24:10'`. Service-day times run
   past 24:00 by design (a trip leaving 23:50 arrives `24:20`, see
   `transit/timeutil.py`).
2. A connection that leaves exactly `min_transfer` seconds after the
   traveller arrives is not offered, e.g. arriving at B at 08:10 with
   `min_transfer=120` the 08:12 departure from B is missed, and with
   `min_transfer=0` a trip leaving B at 08:10 is missed.

Acceptance:

- `transit.timeutil.parse_time` accepts hours `0` to `47` inclusive
  (`"24:05"` -> `86700`, `"47:59:59"` -> `172799`); `"48:00"` and anything
  else malformed raise `ValueError`. Minutes and seconds stay `00`-`59`.
- `transit.timeutil.format_time` does not wrap at midnight:
  `format_time(91800)` -> `"25:30"`, `format_time(86400 + 61)` ->
  `"24:01:01"` (`HH:MM` when the seconds part is zero, `HH:MM:SS` otherwise).
- Boarding rules of `earliest_arrival(timetable, origin, destination,
  depart_at, min_transfer=0)` (times in seconds), all boundaries inclusive:
  at the origin a connection can be boarded if it departs at or after
  `depart_at`; staying on the same trip needs no time; changing to another
  trip at a stop needs `departure >= arrival at that stop + min_transfer`.
- `earliest_arrival` returns a `Journey` (`.arrival`, `.trips`,
  `.connections`) or `None` when the destination cannot be reached.

## Issue 4: Fares are wrong for routes that re-enter a zone

`transit.fares.fare_for_route(network, route, table, concession="adult")`
returns a `Decimal`.

Acceptance:

- The zone count is the number of DISTINCT zones of all stops of the route
  (origin and destination included). A route through zones 1, 2, 1 is a
  2-zone trip, not 3. `count_zones(zones)` returns that number for a list of
  zones (`count_zones(["1", "2", "1"]) == 2`).
- Concession prices are the adult price times `(100 - discount) / 100`,
  rounded to whole cents with halves rounded UP (`ROUND_HALF_UP`): with the
  default table below a 4-zone child fare is `2.325` -> `Decimal("2.33")`.
  Example table: `FareTable({1: "2.40", 2: "3.10", 3: "3.80"}, "0.85")`,
  i.e. 4 zones cost `3.80 + 0.85 = 4.65` for an adult.
- A stop of the route without a zone raises `FareError` with the message
  `stop 'X' has no zone` (X = the first such stop in travel order). An
  unknown concession keeps raising `FareError("unknown concession 'name'")`.

## Issue 5: Itinerary merges legs of the same line and prints long trips badly

`transit.itinerary.build_itinerary(route)` must produce one leg per maximal
run of CONSECUTIVE links on the same line. A route that rides T1, then T2,
then T1 again has three legs, today it gets two (all T1 links are lumped into
one leg). The printed itinerary (`transit.formatting.format_itinerary`) also
shows long durations as e.g. `125 min`.

Acceptance:

- `build_itinerary(route).legs`: one `Leg` per run of consecutive links with
  the same line, in travel order; each leg's `stops` are the stops of that
  run (board ... alight) and its `minutes` the sum of that run's link minutes.
  `Itinerary.changes` is the number of legs minus one.
- `transit.formatting.format_duration(minutes)`: below 60 minutes
  `"<m> min"` (`0` -> `"0 min"`, `59` -> `"59 min"`); from 60 minutes on
  `"<h> h <mm> min"` with two-digit minutes (`60` -> `"1 h 00 min"`,
  `65` -> `"1 h 05 min"`, `600` -> `"10 h 00 min"`).
- `format_itinerary` keeps its layout (see the module docstring of
  `transit/formatting.py`) and uses `format_duration` for the header and
  every leg. Example for a route Central -T1-> Museum -T2-> Park -T1-> Harbour
  with 30, 35 and 5 minutes:

  ```
  Central -> Harbour: 1 h 10 min, 2 changes
    T1   Central -> Museum (1 stop, 30 min)
    T2   Museum -> Park (1 stop, 35 min)
    T1   Park -> Harbour (1 stop, 5 min)
  ```

## Issue 6: Alternative routes contain loops

`transit.alternatives.alternative_routes(network, origin, destination, k=3,
transfer_penalty=0, max_extra=None)` returns routes that go around a loop
(e.g. `A-B-C-A-D`): it only forbids turning straight back.

Acceptance:

- Every returned route is loopless: no stop appears twice in `route.stops`
  (the origin included).
- Routes stay sorted by `Route.key()` and at most `k` are returned; costs use
  the same rule as issue 2 (first boarding is free).
- `max_extra` keeps routes whose cost is AT MOST the best route's cost plus
  `max_extra` (a route costing exactly best + max_extra is kept).
- `k` smaller than 1 raises `ValueError("k must be at least 1")` (before any
  other check). An unknown stop raises `UnknownStopError`; an unreachable
  destination gives `[]`.

## Issue 7: `transit check` reports one-way problems wrongly and always exits 0

`transit.validate.validate_network(network)` treats every link as two-way,
so a stop that can only be left (never entered) through a one-way link is
not reported. It also reports isolated stops twice. `transit.cli main(["check",
FILE])` returns 0 even when problems were printed, so CI never fails.

Acceptance:

- Reachability follows links in their direction of travel: a one-way link
  `A -> B` lets you go from A to B only (use `Network.neighbours`). The root
  is still the smallest stop id among stops that have at least one link.
- A stop without any link is reported only as `isolated stop 'X'`, never also
  as unreachable. Other stops that cannot be reached from the root are
  reported as `stop 'X' cannot be reached from 'R'`.
- `validate_network` returns the messages sorted by the stop id they are
  about (ascending), whichever kind they are.
- `main(["check", path], out=..., err=...)` prints each problem on its own
  line to `out` and returns `1` when there are problems; with no problems it
  prints `OK: <n> stops, <m> links` (m = declared link rows) and returns `0`.
  Errors loading the file still return `2`.
