# transit

A small, dependency-free toolkit for public-transport networks (Python 3.6+,
standard library only).

- `transit/model.py`: stops, links, the `Network` graph and `Route`.
- `transit/loader.py`: the plain-text network format (`[stops]`, `[links]`,
  `[transfers]` sections).
- `transit/dijkstra.py`: shortest routes with transfer penalties.
- `transit/alternatives.py`: k best loopless alternative routes.
- `transit/timeutil.py`, `transit/timetable.py`: service-day times and
  earliest-arrival queries over a timetable (connection scan).
- `transit/fares.py`: zone-based fares and concessions.
- `transit/itinerary.py`, `transit/formatting.py`: legs and printed itineraries.
- `transit/validate.py`, `transit/stats.py`: network checks and statistics.
- `transit/cli.py`: `python3 -m transit.cli route|alternatives|fare|check|stats ...`

Run the tests with

    python3 -m unittest discover -s tests -t .

Open problems are tracked in `ISSUES.md`.
