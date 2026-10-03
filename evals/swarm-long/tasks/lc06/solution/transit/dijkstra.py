"""Shortest routes with Dijkstra's algorithm and transfer penalties.

The cost of a route is the sum of its link minutes plus ``transfer_penalty``
for every line change (see :func:`transit.model.count_changes`: boarding the
first vehicle at the origin is not a change).

Because the penalty depends on the line a stop was reached with, the search
runs over *states* ``(stop, line_arrived_on)`` rather than over bare stops.
The origin state has no line.

Ties
----
Several routes may have the same total cost.  The router must be
deterministic, so ties are broken in this order:

1. fewer line changes;
2. the lexicographically smallest sequence of stop ids;
3. the lexicographically smallest sequence of line names.

That is exactly the ordering of :meth:`transit.model.Route.key`.
"""

import heapq
import itertools

from .errors import NoRouteError, UnknownStopError
from .model import Route

INFINITY = float("inf")


def _check_stops(network, *stop_ids):
    for stop_id in stop_ids:
        if not network.has_stop(stop_id):
            raise UnknownStopError(stop_id)


def shortest_route(network, origin, destination, transfer_penalty=0):
    """Return the cheapest :class:`~transit.model.Route` from origin to destination.

    Raises :class:`UnknownStopError` for an unknown stop id,
    :class:`NoRouteError` when the destination cannot be reached and
    ``ValueError`` for a negative ``transfer_penalty``.
    """
    if transfer_penalty < 0:
        raise ValueError("transfer_penalty must be >= 0")
    _check_stops(network, origin, destination)
    if origin == destination:
        return Route([origin], [], 0)

    # Labels are compared as (cost, changes, stops, lines): the canonical
    # route ordering, so the first label popped at the destination wins.
    counter = itertools.count()
    settled = set()
    heap = [(0, 0, (origin,), (), next(counter), ())]
    while heap:
        cost, changes, stops, lines, _, links = heapq.heappop(heap)
        stop = stops[-1]
        line = lines[-1] if lines else None
        if stop == destination:
            return Route(list(stops), list(links), cost)
        state = (stop, line)
        if state in settled:
            continue
        settled.add(state)
        for link in network.outgoing(stop):
            changed = line is not None and link.line != line
            heapq.heappush(heap, (
                cost + link.minutes + (transfer_penalty if changed else 0),
                changes + (1 if changed else 0),
                stops + (link.b,),
                lines + (link.line,),
                next(counter),
                links + (link,),
            ))
    raise NoRouteError(origin, destination)


def _rebuild(parent, state, cost):
    """Walk the parent pointers back from ``state`` to the origin."""
    links = []
    while state in parent:
        state, link = parent[state]
        links.append(link)
    links.reverse()
    stops = [links[0].a] + [link.b for link in links]
    return Route(stops, links, cost)


def travel_time(network, origin, destination, transfer_penalty=0):
    """Cost of the shortest route (convenience wrapper)."""
    return shortest_route(network, origin, destination, transfer_penalty).cost


def all_costs(network, origin, transfer_penalty=0):
    """Cost of the shortest route from ``origin`` to every reachable stop.

    Returns a dict ``{stop_id: cost}`` (the origin maps to 0).  Unreachable
    stops are left out.
    """
    _check_stops(network, origin)
    result = {}
    for stop_id in network.stop_ids():
        try:
            result[stop_id] = travel_time(network, origin, stop_id, transfer_penalty)
        except NoRouteError:
            pass
    return result
