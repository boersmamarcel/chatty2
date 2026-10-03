"""Command line interface: ``python3 -m transit.cli <command> ...``.

Commands
--------
``route NETWORK FROM TO [--penalty N]``
    Print the itinerary of the shortest route.
``alternatives NETWORK FROM TO [-k N] [--penalty N] [--max-extra N]``
    Print up to k alternative routes, one per line.
``fare NETWORK FROM TO [--prices P1,P2,...] [--extra X] [--concession NAME]``
    Print the fare of the shortest route.
``check NETWORK``
    Validate the network file.
``stats NETWORK``
    Print network statistics.

:func:`main` returns the process exit status instead of calling
``sys.exit`` so it can be tested: 0 on success, 2 when an error was reported.
Errors (any :class:`~transit.errors.TransitError` or unreadable file) are
printed to ``err`` as ``error: <message>``.
"""

import argparse
import sys

from .alternatives import alternative_routes
from .dijkstra import shortest_route
from .errors import TransitError
from .fares import FareTable, fare_for_route, format_price
from .formatting import format_itinerary, format_route
from .itinerary import build_itinerary
from .loader import load_network_file
from .stats import format_stats
from .validate import validate_network

DEFAULT_PRICES = "2.40,3.10,3.80"
DEFAULT_EXTRA = "0.85"


def build_parser():
    parser = argparse.ArgumentParser(prog="transit", description="Transit network tools")
    sub = parser.add_subparsers(dest="command")

    route = sub.add_parser("route", help="shortest route")
    route.add_argument("network")
    route.add_argument("origin")
    route.add_argument("destination")
    route.add_argument("--penalty", type=int, default=0)

    alt = sub.add_parser("alternatives", help="alternative routes")
    alt.add_argument("network")
    alt.add_argument("origin")
    alt.add_argument("destination")
    alt.add_argument("-k", type=int, default=3)
    alt.add_argument("--penalty", type=int, default=0)
    alt.add_argument("--max-extra", type=int, default=None)

    fare = sub.add_parser("fare", help="fare of the shortest route")
    fare.add_argument("network")
    fare.add_argument("origin")
    fare.add_argument("destination")
    fare.add_argument("--prices", default=DEFAULT_PRICES)
    fare.add_argument("--extra", default=DEFAULT_EXTRA)
    fare.add_argument("--concession", default="adult")

    check = sub.add_parser("check", help="validate a network file")
    check.add_argument("network")

    stats = sub.add_parser("stats", help="network statistics")
    stats.add_argument("network")
    return parser


def _fare_table(prices, extra):
    values = [value.strip() for value in prices.split(",") if value.strip()]
    return FareTable(dict((index + 1, value) for index, value in enumerate(values)), extra)


def cmd_route(args, out):
    network = load_network_file(args.network)
    route = shortest_route(network, args.origin.upper(), args.destination.upper(), args.penalty)
    print(format_itinerary(network, build_itinerary(route)), file=out)
    return 0


def cmd_alternatives(args, out):
    network = load_network_file(args.network)
    routes = alternative_routes(network, args.origin.upper(), args.destination.upper(),
                                k=args.k, transfer_penalty=args.penalty,
                                max_extra=args.max_extra)
    if not routes:
        print("no route", file=out)
        return 0
    for index, route in enumerate(routes, start=1):
        print("%d. %s" % (index, format_route(network, route)), file=out)
    return 0


def cmd_fare(args, out):
    network = load_network_file(args.network)
    route = shortest_route(network, args.origin.upper(), args.destination.upper())
    table = _fare_table(args.prices, args.extra)
    price = fare_for_route(network, route, table, args.concession)
    print(format_price(price), file=out)
    return 0


def cmd_check(args, out):
    network = load_network_file(args.network)
    problems = validate_network(network)
    for problem in problems:
        print(problem, file=out)
    if not problems:
        print("OK: %d stops, %d links" % (len(network), network.link_count), file=out)
    return 0


def cmd_stats(args, out):
    network = load_network_file(args.network)
    print(format_stats(network), file=out)
    return 0


COMMANDS = {
    "route": cmd_route,
    "alternatives": cmd_alternatives,
    "fare": cmd_fare,
    "check": cmd_check,
    "stats": cmd_stats,
}


def main(argv=None, out=None, err=None):
    """Run the tool; returns the exit status."""
    out = sys.stdout if out is None else out
    err = sys.stderr if err is None else err
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.command is None:
        parser.print_usage(err)
        return 2
    try:
        return COMMANDS[args.command](args, out)
    except (TransitError, IOError, OSError) as exc:
        print("error: %s" % exc, file=err)
        return 2


if __name__ == "__main__":
    sys.exit(main())
