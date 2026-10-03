"""Consistency checks for a loaded network.

:func:`validate_network` returns a list of human-readable problem strings
(empty when the network is fine).  The checks are:

* isolated stops: stops without any link at all;
* reachability: every stop that has links must be reachable from the *root*
  stop, following links in their direction of travel (one-way links only
  count one way).  The root is the stop with the smallest id among the stops
  that have at least one link.
"""

from collections import deque


def isolated_stops(network):
    """Sorted ids of stops that no link starts or ends at."""
    touched = set()
    for record in network.records:
        touched.add(record.a)
        touched.add(record.b)
    return sorted(stop_id for stop_id in network.stop_ids() if stop_id not in touched)


def root_stop(network):
    """The stop reachability is checked from (None for a network without links)."""
    isolated = set(isolated_stops(network))
    candidates = [stop_id for stop_id in network.stop_ids() if stop_id not in isolated]
    return candidates[0] if candidates else None


def _adjacency(network):
    """Stop id -> set of neighbouring stop ids."""
    adjacency = dict((stop_id, set()) for stop_id in network.stop_ids())
    for record in network.records:
        adjacency[record.a].add(record.b)
        adjacency[record.b].add(record.a)
    return adjacency


def reachable_from(network, start):
    """Set of stop ids reachable from ``start`` (``start`` included)."""
    adjacency = _adjacency(network)
    seen = set([start])
    queue = deque([start])
    while queue:
        here = queue.popleft()
        for nxt in sorted(adjacency[here]):
            if nxt not in seen:
                seen.add(nxt)
                queue.append(nxt)
    return seen


def unreachable_stops(network):
    """Sorted ids of stops that cannot be reached from the root stop."""
    root = root_stop(network)
    if root is None:
        return []
    seen = reachable_from(network, root)
    return sorted(stop_id for stop_id in network.stop_ids() if stop_id not in seen)


def validate_network(network):
    """Every problem found in ``network``, as a list of strings."""
    problems = []
    for stop_id in isolated_stops(network):
        problems.append("isolated stop %r" % (stop_id,))
    root = root_stop(network)
    for stop_id in unreachable_stops(network):
        problems.append("stop %r cannot be reached from %r" % (stop_id, root))
    return problems


def is_valid(network):
    """True when :func:`validate_network` finds nothing."""
    return not validate_network(network)
