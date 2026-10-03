"""Zone-based fares.

Every stop belongs to a fare zone (``Stop.zone``, a short string such as
``"1"``, ``"2"`` or ``"A"``).  The price of a trip depends only on the number
of *distinct* zones the route passes through, origin and destination
included.  A route that leaves a zone and comes back into it pays for that
zone once.

Prices are kept as :class:`decimal.Decimal` amounts.  A :class:`FareTable`
maps a zone count to a price; counts above the largest listed count cost the
largest listed price plus ``extra_zone`` for every further zone.

Concessions are percentage discounts on the adult price.  A discounted price
is rounded to whole cents, halves rounded up (0.005 -> 0.01).
"""

from decimal import Decimal, ROUND_HALF_UP

from .errors import FareError

CENT = Decimal("0.01")

DEFAULT_CONCESSIONS = {
    "adult": 0,
    "child": 50,
    "senior": 35,
}


def to_money(value):
    """Convert ``value`` (str, int or Decimal) to a Decimal amount of money."""
    if isinstance(value, float):
        raise TypeError("use strings or Decimal for money, not float")
    amount = Decimal(value)
    if amount < 0:
        raise FareError("negative price %s" % amount)
    return amount


class FareTable(object):
    """Prices per number of zones, plus concession discounts.

    ``prices`` maps zone counts (int >= 1) to prices; the counts must be
    consecutive starting at 1.  ``extra_zone`` is added per zone beyond the
    largest count.  ``concessions`` maps a concession name to a whole
    percentage discount (0-100).
    """

    def __init__(self, prices, extra_zone="0.00", concessions=None):
        if not prices:
            raise FareError("a fare table needs at least one price")
        counts = sorted(prices)
        if counts != list(range(1, len(counts) + 1)):
            raise FareError("zone counts must be 1..n without gaps, got %s" % counts)
        self.prices = dict((count, to_money(prices[count])) for count in counts)
        self.extra_zone = to_money(extra_zone)
        self.concessions = dict(DEFAULT_CONCESSIONS if concessions is None else concessions)
        for name, percent in self.concessions.items():
            if not 0 <= percent <= 100:
                raise FareError("concession %r: discount must be 0-100, got %r" % (name, percent))

    @property
    def max_listed(self):
        return max(self.prices)

    def price_for_zones(self, zones):
        """Adult price for a trip through ``zones`` distinct zones."""
        if zones < 1:
            raise FareError("a trip covers at least one zone")
        if zones in self.prices:
            return self.prices[zones]
        top = self.max_listed
        return self.prices[top] + self.extra_zone * (zones - top)

    def discounted(self, price, concession):
        """``price`` with the concession discount applied, in whole cents."""
        try:
            percent = self.concessions[concession]
        except KeyError:
            raise FareError("unknown concession %r" % (concession,))
        factor = Decimal(100 - percent) / Decimal(100)
        return (price * factor).quantize(CENT)


def zones_of_route(network, route):
    """Zone of every stop of ``route``, in travel order."""
    return [network.stop(stop_id).zone for stop_id in route.stops]


def count_zones(zones):
    """Number of zones a sequence of stop zones is charged for."""
    if not zones:
        return 0
    count = 1
    for previous, current in zip(zones, zones[1:]):
        if current != previous:
            count += 1
    return count


def fare_for_route(network, route, table, concession="adult"):
    """Price (Decimal) of travelling ``route`` with ``concession``.

    Raises :class:`FareError` for an unknown concession or a stop without a
    zone.
    """
    zones = zones_of_route(network, route)
    adult = table.price_for_zones(count_zones(zones))
    return table.discounted(adult, concession)


def fare_matrix(network, routes, table, concession="adult"):
    """``{(origin, destination): price}`` for a list of routes."""
    result = {}
    for route in routes:
        result[(route.origin, route.destination)] = fare_for_route(
            network, route, table, concession)
    return result


def format_price(amount, currency="EUR"):
    """``Decimal('3.1')`` -> ``'EUR 3.10'``."""
    return "%s %s" % (currency, amount.quantize(CENT, rounding=ROUND_HALF_UP))
