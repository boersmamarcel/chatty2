"""Timetables and earliest-arrival queries (connection scan).

Timetable format
----------------
A CSV text with a header row ``trip,line,stop,arrive,depart`` and one row per
stop call, the calls of one trip in travel order::

    trip,line,stop,arrive,depart
    T1,L1,A,,07:58
    T1,L1,B,08:03,08:04
    T1,L1,C,08:10,

``arrive`` may be empty for the first call of a trip and ``depart`` for the
last; when only one of the two is given for an intermediate call, it is used
for both.  Times are service-day times (see :mod:`transit.timeutil`).

Every pair of consecutive calls of a trip becomes a :class:`Connection`
(departing the first stop, arriving at the second).

Boarding rules used by :func:`earliest_arrival`
-----------------------------------------------
* At the origin a connection can be boarded when it departs at or after the
  requested departure time.
* Staying on the same trip never needs any extra time.
* Changing to another trip at stop ``s`` needs ``min_transfer`` seconds: the
  next connection can be boarded when it departs at or after the arrival at
  ``s`` plus ``min_transfer``.
"""

import csv
import io

from .errors import TimetableError
from .timeutil import format_time, parse_optional_time

COLUMNS = ("trip", "line", "stop", "arrive", "depart")


class Connection(object):
    """One vehicle hop: ``trip`` leaves ``from_stop`` at ``dep``, reaches ``to_stop`` at ``arr``."""

    __slots__ = ("trip", "line", "from_stop", "to_stop", "dep", "arr", "seq")

    def __init__(self, trip, line, from_stop, to_stop, dep, arr, seq):
        self.trip = trip
        self.line = line
        self.from_stop = from_stop
        self.to_stop = to_stop
        self.dep = dep
        self.arr = arr
        self.seq = seq

    def key(self):
        return (self.dep, self.arr, self.trip, self.seq)

    def __repr__(self):
        return "Connection(%s %s %s %s -> %s %s)" % (
            self.trip, self.line, self.from_stop, format_time(self.dep),
            self.to_stop, format_time(self.arr))


class Timetable(object):
    """All connections of a timetable, sorted by departure time."""

    def __init__(self, connections):
        self.connections = sorted(connections, key=Connection.key)
        self._by_trip = {}
        for conn in sorted(connections, key=lambda c: (c.trip, c.seq)):
            self._by_trip.setdefault(conn.trip, []).append(conn)

    def trips(self):
        return sorted(self._by_trip)

    def trip_connections(self, trip):
        """Connections of one trip in travel order."""
        return list(self._by_trip.get(trip, []))

    def stops(self):
        found = set()
        for conn in self.connections:
            found.add(conn.from_stop)
            found.add(conn.to_stop)
        return sorted(found)

    def departures(self, stop, after=0):
        """Connections leaving ``stop`` at or after ``after``, by time."""
        return [c for c in self.connections if c.from_stop == stop and c.dep >= after]


def load_timetable(text):
    """Parse the timetable CSV text into a :class:`Timetable`.

    Raises :class:`TimetableError` (message prefixed with ``line N:``) on the
    first malformed row.
    """
    reader = csv.reader(io.StringIO(text))
    calls = {}
    order = []
    header_seen = False
    for lineno, row in enumerate(reader, start=1):
        row = [field.strip() for field in row]
        if not row or not any(row) or row[0].startswith("#"):
            continue
        if not header_seen:
            if tuple(field.lower() for field in row) != COLUMNS:
                raise TimetableError("line %d: expected header %s" % (lineno, ",".join(COLUMNS)))
            header_seen = True
            continue
        if len(row) != len(COLUMNS):
            raise TimetableError("line %d: expected %d fields, got %d"
                                 % (lineno, len(COLUMNS), len(row)))
        trip, line, stop, arrive, depart = row
        try:
            arr = parse_optional_time(arrive)
            dep = parse_optional_time(depart)
        except ValueError as exc:
            raise TimetableError("line %d: %s" % (lineno, exc))
        if arr is None and dep is None:
            raise TimetableError("line %d: call without any time" % lineno)
        if trip not in calls:
            calls[trip] = []
            order.append(trip)
        calls[trip].append((lineno, line, stop, arr, dep))

    connections = []
    for trip in order:
        trip_calls = calls[trip]
        for index, (first, second) in enumerate(zip(trip_calls, trip_calls[1:])):
            lineno, line, stop, arr1, dep1 = first
            lineno2, _line2, stop2, arr2, dep2 = second
            dep = dep1 if dep1 is not None else arr1
            arr = arr2 if arr2 is not None else dep2
            if arr < dep:
                raise TimetableError("line %d: trip %s arrives at %s before it departs %s"
                                     % (lineno2, trip, stop2, stop))
            connections.append(Connection(trip, line, stop, stop2, dep, arr, index))
    return Timetable(connections)


class Journey(object):
    """Result of an earliest-arrival query: the connections ridden, in order."""

    def __init__(self, connections, depart_at):
        self.connections = list(connections)
        self.depart_at = depart_at

    @property
    def arrival(self):
        if not self.connections:
            return self.depart_at
        return self.connections[-1].arr

    @property
    def trips(self):
        """Trip ids in riding order (each trip once per boarding)."""
        result = []
        for conn in self.connections:
            if not result or result[-1] != conn.trip:
                result.append(conn.trip)
        return result

    @property
    def transfers(self):
        return max(len(self.trips) - 1, 0)

    def describe(self):
        """One line per trip ridden: ``T1 L1 A 07:58 -> C 08:10``."""
        lines = []
        index = 0
        conns = self.connections
        while index < len(conns):
            start = conns[index]
            end = start
            while index + 1 < len(conns) and conns[index + 1].trip == start.trip:
                index += 1
                end = conns[index]
            lines.append("%s %s %s %s -> %s %s" % (
                start.trip, start.line, start.from_stop, format_time(start.dep),
                end.to_stop, format_time(end.arr)))
            index += 1
        return "\n".join(lines)


def earliest_arrival(timetable, origin, destination, depart_at, min_transfer=0):
    """Earliest arrival at ``destination`` leaving ``origin`` at ``depart_at``.

    Returns a :class:`Journey` or ``None`` when the destination cannot be
    reached that service day.  ``min_transfer`` is in seconds.
    """
    if min_transfer < 0:
        raise ValueError("min_transfer must be >= 0")
    if origin == destination:
        return Journey([], depart_at)

    arrival = {origin: depart_at}
    # stop -> (enter connection, exit connection) of the trip that got us there
    reached_by = {}
    # trip -> connection where we boarded it
    boarded = {}
    for conn in timetable.connections:
        if conn.dep < depart_at:
            continue
        if conn.trip not in boarded:
            reached = arrival.get(conn.from_stop)
            if reached is None:
                continue
            if conn.from_stop == origin:
                ok = conn.dep >= depart_at
            else:
                ok = conn.dep >= reached + min_transfer
            if not ok:
                continue
            boarded[conn.trip] = conn
        if conn.arr < arrival.get(conn.to_stop, float("inf")):
            arrival[conn.to_stop] = conn.arr
            reached_by[conn.to_stop] = (boarded[conn.trip], conn)

    if destination not in reached_by:
        return None
    return Journey(_rebuild(timetable, reached_by, origin, destination), depart_at)


def _rebuild(timetable, reached_by, origin, destination):
    """Connections from origin to destination following ``reached_by``."""
    segments = []
    stop = destination
    guard = 0
    while stop != origin:
        enter, exit_ = reached_by[stop]
        trip = timetable.trip_connections(enter.trip)
        segments.append([c for c in trip if enter.seq <= c.seq <= exit_.seq])
        stop = enter.from_stop
        guard += 1
        if guard > len(timetable.connections):
            raise RuntimeError("journey reconstruction did not terminate")
    result = []
    for segment in reversed(segments):
        result.extend(segment)
    return result
