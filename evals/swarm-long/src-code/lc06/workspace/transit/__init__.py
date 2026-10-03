"""transit: a small public-transport routing toolkit.

Load a network (:func:`transit.loader.load_network`), find shortest routes
with transfer penalties (:mod:`transit.dijkstra`), alternatives
(:mod:`transit.alternatives`), earliest arrivals in a timetable
(:mod:`transit.timetable`), zone fares (:mod:`transit.fares`) and print
itineraries (:mod:`transit.formatting`).
"""

from .errors import (FareError, NetworkFormatError, NoRouteError, TimetableError,  # noqa: F401
                     TransitError, UnknownStopError)
from .model import Link, Network, Route, Stop, count_changes, route_from_path  # noqa: F401

__version__ = "0.4.2"
