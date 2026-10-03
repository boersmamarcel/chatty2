"""Core data model: stops, links, the network graph and routes.

A :class:`Network` is a directed multigraph.  Stops are identified by a short
string id (``"CEN"``, ``"B"``...).  A *link* is one hop between two adjacent
stops served by one line and takes a whole, positive number of minutes.  A
link record that is not one-way is stored as two directed :class:`Link`
objects, one per direction.

A :class:`Route` is a concrete path through the network: the stops visited in
order, the links used between them and the total cost the router assigned to
it (minutes plus any transfer penalties).

Line changes
------------
Throughout the package a *line change* (or *transfer*) happens between two
consecutive links of a route whose lines differ.  Boarding the first vehicle
at the origin is not a change.  :func:`count_changes` implements exactly that
rule and is the single source of truth for it.
"""

from collections import OrderedDict

from .errors import UnknownStopError


class Stop(object):
    """A stop (station, platform group) of the network."""

    __slots__ = ("id", "name", "zone")

    def __init__(self, stop_id, name, zone=None):
        self.id = stop_id
        self.name = name
        self.zone = zone

    def __repr__(self):
        return "Stop(%r, %r, zone=%r)" % (self.id, self.name, self.zone)

    def __eq__(self, other):
        if not isinstance(other, Stop):
            return NotImplemented
        return (self.id, self.name, self.zone) == (other.id, other.name, other.zone)

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result

    def __hash__(self):
        return hash((self.id, self.name, self.zone))


class Link(object):
    """One directed hop from stop ``a`` to stop ``b`` on ``line``."""

    __slots__ = ("a", "b", "line", "minutes")

    def __init__(self, a, b, line, minutes):
        self.a = a
        self.b = b
        self.line = line
        self.minutes = minutes

    def key(self):
        """Sort key used to make iteration over links deterministic."""
        return (self.b, self.line, self.minutes)

    def __repr__(self):
        return "Link(%r, %r, %r, %r)" % (self.a, self.b, self.line, self.minutes)

    def __eq__(self, other):
        if not isinstance(other, Link):
            return NotImplemented
        return (self.a, self.b, self.line, self.minutes) == (
            other.a, other.b, other.line, other.minutes)

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result

    def __hash__(self):
        return hash((self.a, self.b, self.line, self.minutes))


class LinkRecord(object):
    """A link as it was declared (possibly bidirectional)."""

    __slots__ = ("a", "b", "line", "minutes", "oneway")

    def __init__(self, a, b, line, minutes, oneway):
        self.a = a
        self.b = b
        self.line = line
        self.minutes = minutes
        self.oneway = oneway

    def __repr__(self):
        return "LinkRecord(%r, %r, %r, %r, oneway=%r)" % (
            self.a, self.b, self.line, self.minutes, self.oneway)


def count_changes(lines):
    """Number of line changes in a sequence of line names.

    >>> count_changes(["L1", "L1", "L2", "L1"])
    2
    >>> count_changes([])
    0
    """
    changes = 0
    previous = None
    for line in lines:
        if previous is not None and line != previous:
            changes += 1
        previous = line
    return changes


class Network(object):
    """A transit network: stops, directed links and per-stop transfer times."""

    def __init__(self):
        self._stops = OrderedDict()
        self._out = {}
        self._transfer = {}
        self.records = []

    # -- stops ---------------------------------------------------------

    def add_stop(self, stop_id, name, zone=None):
        """Add a stop.  Raises ``ValueError`` if the id is already used."""
        if stop_id in self._stops:
            raise ValueError("duplicate stop %r" % (stop_id,))
        stop = Stop(stop_id, name, zone)
        self._stops[stop_id] = stop
        self._out[stop_id] = []
        return stop

    def stop(self, stop_id):
        """Return the :class:`Stop` with this id or raise UnknownStopError."""
        try:
            return self._stops[stop_id]
        except KeyError:
            raise UnknownStopError(stop_id)

    def has_stop(self, stop_id):
        return stop_id in self._stops

    def __contains__(self, stop_id):
        return stop_id in self._stops

    def __len__(self):
        return len(self._stops)

    def stops(self):
        """Every stop, in the order they were added."""
        return list(self._stops.values())

    def stop_ids(self):
        """Every stop id, sorted."""
        return sorted(self._stops)

    def name(self, stop_id):
        """Display name of a stop."""
        return self.stop(stop_id).name

    # -- links ---------------------------------------------------------

    def add_link(self, a, b, line, minutes, oneway=False):
        """Declare a link between two existing stops.

        ``minutes`` must be a positive integer.  Unless ``oneway`` is true the
        link can be travelled in both directions.
        """
        for stop_id in (a, b):
            if stop_id not in self._stops:
                raise UnknownStopError(stop_id)
        if not isinstance(minutes, int) or isinstance(minutes, bool) or minutes <= 0:
            raise ValueError("minutes must be a positive integer, got %r" % (minutes,))
        if a == b:
            raise ValueError("a link cannot start and end at the same stop %r" % (a,))
        self.records.append(LinkRecord(a, b, line, minutes, bool(oneway)))
        self._out[a].append(Link(a, b, line, minutes))
        if not oneway:
            self._out[b].append(Link(b, a, line, minutes))

    @property
    def link_count(self):
        """Number of declared link records (a two-way link counts once)."""
        return len(self.records)

    def outgoing(self, stop_id):
        """Directed links leaving ``stop_id``, sorted by (to, line, minutes)."""
        if stop_id not in self._out:
            raise UnknownStopError(stop_id)
        return sorted(self._out[stop_id], key=Link.key)

    def neighbours(self, stop_id):
        """Sorted ids of the stops directly reachable from ``stop_id``."""
        return sorted(set(link.b for link in self.outgoing(stop_id)))

    def degree(self, stop_id):
        """Number of directed links touching the stop (in + out)."""
        out = len(self.outgoing(stop_id))
        inc = sum(1 for links in self._out.values() for link in links if link.b == stop_id)
        return out + inc

    def lines(self):
        """Sorted names of every line used by at least one link."""
        return sorted(set(record.line for record in self.records))

    def find_link(self, a, b, line):
        """The fastest directed link a -> b on ``line`` (None if there is none)."""
        best = None
        for link in self.outgoing(a):
            if link.b == b and link.line == line:
                if best is None or link.minutes < best.minutes:
                    best = link
        return best

    # -- transfers -----------------------------------------------------

    def set_transfer_time(self, stop_id, minutes):
        """Minimum time to change lines at ``stop_id`` (informational)."""
        if stop_id not in self._stops:
            raise UnknownStopError(stop_id)
        if minutes < 0:
            raise ValueError("transfer time cannot be negative")
        self._transfer[stop_id] = minutes

    def transfer_time(self, stop_id, default=0):
        """Transfer time at ``stop_id``; ``default`` when none was declared."""
        if stop_id not in self._stops:
            raise UnknownStopError(stop_id)
        return self._transfer.get(stop_id, default)


class Route(object):
    """A path through the network as found by a router.

    ``stops`` is the list of stop ids visited (origin first), ``links`` the
    list of :class:`Link` objects used (one fewer than stops) and ``cost`` the
    total cost the router assigned (minutes plus transfer penalties).
    """

    def __init__(self, stops, links, cost):
        if len(links) != max(len(stops) - 1, 0):
            raise ValueError("a route with %d stops needs %d links, got %d"
                             % (len(stops), max(len(stops) - 1, 0), len(links)))
        self.stops = list(stops)
        self.links = list(links)
        self.cost = cost

    @property
    def origin(self):
        return self.stops[0]

    @property
    def destination(self):
        return self.stops[-1]

    @property
    def lines(self):
        """Line of every link, in travel order."""
        return [link.line for link in self.links]

    @property
    def minutes(self):
        """Pure travel time (no penalties)."""
        return sum(link.minutes for link in self.links)

    @property
    def changes(self):
        """Number of line changes (see :func:`count_changes`)."""
        return count_changes(self.lines)

    def key(self):
        """The canonical ordering of routes: cost, changes, stops, lines."""
        return (self.cost, self.changes, tuple(self.stops), tuple(self.lines))

    def __eq__(self, other):
        if not isinstance(other, Route):
            return NotImplemented
        return self.key() == other.key()

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result

    def __hash__(self):
        return hash(self.key())

    def __repr__(self):
        return "Route(%s, cost=%r, lines=%s)" % (
            "-".join(self.stops), self.cost, "/".join(self.lines))


def route_from_path(network, stops, lines, transfer_penalty=0):
    """Build a :class:`Route` from stop ids and the line used on each hop.

    The cost is computed with the package-wide rule: minutes of every link
    plus ``transfer_penalty`` per line change.  Raises ``ValueError`` when a
    hop does not exist in the network.
    """
    if len(lines) != max(len(stops) - 1, 0):
        raise ValueError("need exactly one line per hop")
    links = []
    for a, b, line in zip(stops, stops[1:], lines):
        link = network.find_link(a, b, line)
        if link is None:
            raise ValueError("no link %s -> %s on line %s" % (a, b, line))
        links.append(link)
    cost = sum(link.minutes for link in links) + transfer_penalty * count_changes(lines)
    return Route(stops, links, cost)
