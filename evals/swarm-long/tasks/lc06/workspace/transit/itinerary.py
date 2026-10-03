"""Turn a route into an itinerary of legs.

A *leg* is a maximal run of consecutive links of a route that use the same
line: the traveller boards at the first stop of the run and alights at the
last one.  A route that uses line T1, then T2, then T1 again therefore has
three legs (the traveller rides T1 twice).

Penalties are not part of an itinerary: its duration is the pure travel time,
the sum of the link minutes.
"""

from collections import OrderedDict


class Leg(object):
    """A ride on one line from ``board`` to ``alight``."""

    __slots__ = ("line", "stops", "minutes")

    def __init__(self, line, stops, minutes):
        if len(stops) < 2:
            raise ValueError("a leg visits at least two stops")
        self.line = line
        self.stops = list(stops)
        self.minutes = minutes

    @classmethod
    def from_links(cls, links):
        """Build a leg from consecutive links that all use one line."""
        if not links:
            raise ValueError("a leg needs at least one link")
        line = links[0].line
        stops = [links[0].a] + [link.b for link in links]
        return cls(line, stops, sum(link.minutes for link in links))

    @property
    def board(self):
        return self.stops[0]

    @property
    def alight(self):
        return self.stops[-1]

    @property
    def hops(self):
        """Number of stops travelled (stops visited minus the boarding stop)."""
        return len(self.stops) - 1

    def __repr__(self):
        return "Leg(%s %s->%s, %d hops, %d min)" % (
            self.line, self.board, self.alight, self.hops, self.minutes)

    def __eq__(self, other):
        if not isinstance(other, Leg):
            return NotImplemented
        return (self.line, self.stops, self.minutes) == (other.line, other.stops, other.minutes)

    def __ne__(self, other):
        result = self.__eq__(other)
        if result is NotImplemented:
            return result
        return not result


class Itinerary(object):
    """Legs of a route, in travel order."""

    def __init__(self, origin, destination, legs):
        self.origin = origin
        self.destination = destination
        self.legs = list(legs)

    @property
    def minutes(self):
        """Pure travel time of the whole itinerary."""
        return sum(leg.minutes for leg in self.legs)

    @property
    def changes(self):
        """Number of times the traveller changes vehicle."""
        return max(len(self.legs) - 1, 0)

    @property
    def lines(self):
        """Line of every leg, in order (a line may appear more than once)."""
        return [leg.line for leg in self.legs]

    def transfer_stops(self):
        """Stops where the traveller changes vehicle, in order."""
        return [leg.alight for leg in self.legs[:-1]]

    def __repr__(self):
        return "Itinerary(%s->%s, %r)" % (self.origin, self.destination, self.legs)


def group_links(links):
    """Split route links into runs that share one line."""
    groups = OrderedDict()
    for link in links:
        groups.setdefault(link.line, []).append(link)
    return list(groups.values())


def build_itinerary(route):
    """Build the :class:`Itinerary` of a :class:`~transit.model.Route`.

    A route with a single stop (origin == destination) has no legs.
    """
    legs = [Leg.from_links(run) for run in group_links(route.links)]
    return Itinerary(route.origin, route.destination, legs)


def total_wait(itinerary, network, default=0):
    """Sum of the declared transfer times at every transfer stop (minutes)."""
    return sum(network.transfer_time(stop, default) for stop in itinerary.transfer_stops())
