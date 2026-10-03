"""Descriptive statistics of a network (used by ``transit stats``).

All averages are :class:`decimal.Decimal` values rounded to one decimal
place, halves rounded up, so that the printed report is stable across
platforms.
"""

from decimal import Decimal, ROUND_HALF_UP

ONE_PLACE = Decimal("0.1")


def _average(values):
    if not values:
        return Decimal("0.0")
    total = Decimal(sum(values))
    return (total / Decimal(len(values))).quantize(ONE_PLACE, rounding=ROUND_HALF_UP)


def line_lengths(network):
    """``{line: number of link records}``."""
    result = {}
    for record in network.records:
        result[record.line] = result.get(record.line, 0) + 1
    return result


def line_minutes(network):
    """``{line: total minutes of its link records}`` (one direction)."""
    result = {}
    for record in network.records:
        result[record.line] = result.get(record.line, 0) + record.minutes
    return result


def busiest_stops(network, top=3):
    """Ids of the ``top`` stops with the highest degree.

    Ties are broken by stop id (ascending).  Stops with degree 0 are never
    listed.
    """
    scored = [(-network.degree(stop_id), stop_id) for stop_id in network.stop_ids()]
    scored = [item for item in scored if item[0] < 0]
    scored.sort()
    return [stop_id for _score, stop_id in scored[:top]]


def zone_sizes(network):
    """``{zone: number of stops}``; stops without a zone are counted under ``None``."""
    result = {}
    for stop in network.stops():
        result[stop.zone] = result.get(stop.zone, 0) + 1
    return result


def network_stats(network):
    """A dict with the headline numbers of the network."""
    minutes = [record.minutes for record in network.records]
    return {
        "stops": len(network),
        "links": network.link_count,
        "lines": len(network.lines()),
        "oneway_links": sum(1 for record in network.records if record.oneway),
        "average_link_minutes": _average(minutes),
        "longest_link_minutes": max(minutes) if minutes else 0,
        "busiest_stops": busiest_stops(network),
    }


def format_stats(network):
    """The ``transit stats`` report text."""
    stats = network_stats(network)
    lines = [
        "stops: %d" % stats["stops"],
        "links: %d (%d one-way)" % (stats["links"], stats["oneway_links"]),
        "lines: %d" % stats["lines"],
        "average link: %s min" % stats["average_link_minutes"],
        "longest link: %d min" % stats["longest_link_minutes"],
        "busiest stops: %s" % (", ".join(stats["busiest_stops"]) or "-"),
    ]
    lengths = line_lengths(network)
    totals = line_minutes(network)
    for line in sorted(lengths):
        lines.append("line %s: %d links, %d min" % (line, lengths[line], totals[line]))
    return "\n".join(lines)
