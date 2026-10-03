"""Alternative routes: the k best loopless routes between two stops.

The search is a best-first enumeration of partial routes ordered by the
canonical route ordering (:meth:`transit.model.Route.key`: cost, then line
changes, then stop ids, then line names).  Because every link takes at least
one minute, extending a partial route always makes it strictly more
expensive, so complete routes come out of the queue already in canonical
order and the first ``k`` of them are the answer.

Costs use the package-wide rule: minutes plus ``transfer_penalty`` per line
change, boarding the first vehicle being free.

Two routes are different alternatives when their stop sequences or their line
sequences differ: riding the same stops on a parallel line is an alternative.
"""

import heapq
import itertools

from .errors import UnknownStopError
from .model import Route


def alternative_routes(network, origin, destination, k=3, transfer_penalty=0, max_extra=None):
    """Return up to ``k`` loopless routes from ``origin`` to ``destination``.

    * Routes are sorted by :meth:`~transit.model.Route.key`.
    * ``max_extra`` (minutes of cost, optional) drops routes that cost more
      than the best route plus ``max_extra``.
    * An empty list means the destination cannot be reached.
    """
    if k < 1:
        raise ValueError("k must be at least 1")
    for stop_id in (origin, destination):
        if not network.has_stop(stop_id):
            raise UnknownStopError(stop_id)
    if transfer_penalty < 0:
        raise ValueError("transfer_penalty must be >= 0")
    if origin == destination:
        return [Route([origin], [], 0)][:k]

    counter = itertools.count()
    heap = [(0, 0, (origin,), (), next(counter), ())]
    results = []
    best_cost = None
    while heap and len(results) < k:
        cost, changes, stops, lines, _, links = heapq.heappop(heap)
        if best_cost is not None and max_extra is not None and cost > best_cost + max_extra:
            break
        here = stops[-1]
        if here == destination:
            if best_cost is None:
                best_cost = cost
            results.append(Route(list(stops), list(links), cost))
            continue
        last_line = lines[-1] if lines else None
        for link in network.outgoing(here):
            if link.b in stops:
                continue  # loopless: never visit a stop twice
            changed = last_line is not None and link.line != last_line
            new_cost = cost + link.minutes + (transfer_penalty if changed else 0)
            heapq.heappush(heap, (
                new_cost,
                changes + (1 if changed else 0),
                stops + (link.b,),
                lines + (link.line,),
                next(counter),
                links + (link,),
            ))
    return results


def route_overlap(first, second):
    """Fraction (0..1) of the links of ``first`` that ``second`` also uses.

    Links are compared by (from, to) regardless of the line.  An empty first
    route overlaps 0.
    """
    if not first.links:
        return 0.0
    hops = set((link.a, link.b) for link in second.links)
    shared = sum(1 for link in first.links if (link.a, link.b) in hops)
    return shared / float(len(first.links))


def diverse_routes(network, origin, destination, k=3, transfer_penalty=0, max_overlap=0.5,
                   pool=10):
    """Up to ``k`` routes that overlap the already chosen ones at most ``max_overlap``.

    Candidates are the best ``pool`` alternatives, considered in order; the
    best route is always chosen.
    """
    chosen = []
    for route in alternative_routes(network, origin, destination, k=pool,
                                    transfer_penalty=transfer_penalty):
        if all(route_overlap(route, other) <= max_overlap for other in chosen):
            chosen.append(route)
        if len(chosen) == k:
            break
    return chosen
