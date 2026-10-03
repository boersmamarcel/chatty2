"""Human-readable text for itineraries, routes and durations.

The itinerary text is used by the command line tool and by the journey
planner e-mails, so its layout is fixed::

    Central -> Harbour: 1 h 05 min, 1 change
      T1   Central -> Museum (2 stops, 25 min)
      T2   Museum -> Harbour (3 stops, 40 min)

* Header: ``<origin name> -> <destination name>: <duration>, <changes>``.
* One line per leg: two spaces, the line name left-aligned in a field of
  four characters, one space, ``<board name> -> <alight name>``, then
  `` (<hops> stop(s), <duration>)``.
* Durations: see :func:`format_duration`.  Changes: see :func:`format_changes`.
"""


def plural(count, word, plural_word=None):
    """``plural(1, "stop")`` -> ``"1 stop"``; ``plural(2, "stop")`` -> ``"2 stops"``."""
    if count == 1:
        return "%d %s" % (count, word)
    return "%d %s" % (count, plural_word or word + "s")


def format_duration(minutes):
    """Render a whole number of minutes.

    Durations are always given in minutes, e.g. ``"45 min"``.
    """
    if minutes < 0:
        raise ValueError("negative duration %r" % (minutes,))
    return "%d min" % minutes


def format_changes(changes):
    """``0`` -> ``"no changes"``, ``1`` -> ``"1 change"``, ``3`` -> ``"3 changes"``."""
    if changes == 0:
        return "no changes"
    return plural(changes, "change")


def format_leg(network, leg):
    """One indented line describing a leg (see the module docstring)."""
    return "  %-4s %s -> %s (%s, %s)" % (
        leg.line,
        network.name(leg.board),
        network.name(leg.alight),
        plural(leg.hops, "stop"),
        format_duration(leg.minutes),
    )


def format_itinerary(network, itinerary):
    """Multi-line text of an :class:`~transit.itinerary.Itinerary`."""
    header = "%s -> %s: %s, %s" % (
        network.name(itinerary.origin),
        network.name(itinerary.destination),
        format_duration(itinerary.minutes),
        format_changes(itinerary.changes),
    )
    lines = [header]
    for leg in itinerary.legs:
        lines.append(format_leg(network, leg))
    return "\n".join(lines)


def format_route(network, route):
    """Compact one-line form: ``Central -T1-> Museum -T2-> Harbour [65]``."""
    if not route.links:
        return "%s [%s]" % (network.name(route.origin), route.cost)
    parts = [network.name(route.stops[0])]
    for link in route.links:
        parts.append("-%s-> %s" % (link.line, network.name(link.b)))
    return "%s [%s]" % (" ".join(parts), route.cost)


def format_table(rows, headers):
    """Plain text table with left-aligned columns separated by two spaces."""
    table = [list(map(str, headers))] + [list(map(str, row)) for row in rows]
    widths = [max(len(row[i]) for row in table) for i in range(len(headers))]
    out = []
    for index, row in enumerate(table):
        out.append("  ".join(cell.ljust(widths[i]) for i, cell in enumerate(row)).rstrip())
        if index == 0:
            out.append("  ".join("-" * w for w in widths))
    return "\n".join(out)
