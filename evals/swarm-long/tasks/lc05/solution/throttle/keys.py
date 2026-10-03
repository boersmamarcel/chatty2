"""Limiter key normalisation.

Requests are limited per *key*: a tenant plus the route they called. Two
requests that should share a budget must map to the same key, so tenants and
routes are normalised first:

* the tenant is stripped of surrounding whitespace and lowercased;
* the route is lowercased and every purely numeric path segment is replaced
  by the placeholder ``{id}``, so ``/v1/users/17`` and ``/v1/users/42`` share
  one budget.

The key itself is ``"<tenant>:<route>"``.
"""

import re

from .errors import InvalidKeyError

_UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")

#: Placeholder that replaces identifier segments in routes.
ID_PLACEHOLDER = "{id}"

#: Separator between tenant and route in a key.
SEPARATOR = ":"


def normalise_tenant(tenant):
    """Return the canonical form of a tenant name.

    Raises :class:`InvalidKeyError` (message ``tenant must not be empty``)
    when nothing is left after stripping, and when the tenant contains the
    key separator ``":"`` (message ``tenant must not contain ':'``).
    """
    if tenant is None:
        raise InvalidKeyError("tenant must not be empty")
    cleaned = str(tenant).strip().lower()
    if not cleaned:
        raise InvalidKeyError("tenant must not be empty")
    if SEPARATOR in cleaned:
        raise InvalidKeyError("tenant must not contain '%s'" % SEPARATOR)
    return cleaned


def _is_identifier(segment):
    """True when a path segment is an identifier that should become ``{id}``."""
    return segment.isdigit() or _UUID.match(segment) is not None


def normalise_route(route):
    """Return the canonical form of a request route (see module docstring)."""
    if route is None:
        return "/"
    path = str(route).strip()
    for mark in "?#":
        path = path.split(mark, 1)[0]
    path = path.lower()
    out = []
    for segment in path.split("/"):
        if not segment:
            continue
        if _is_identifier(segment):
            out.append(ID_PLACEHOLDER)
        else:
            out.append(segment)
    return "/" + "/".join(out)


def normalise_key(tenant, route):
    """Build the limiter key for ``tenant`` calling ``route``.

    >>> normalise_key(" Acme ", "/v1/Users/17")
    'acme:/v1/users/{id}'
    """
    return "%s%s%s" % (normalise_tenant(tenant), SEPARATOR, normalise_route(route))


def split_key(key):
    """Inverse of :func:`normalise_key`: ``(tenant, route)``.

    Raises :class:`InvalidKeyError` when ``key`` has no separator.
    """
    tenant, sep, route = key.partition(SEPARATOR)
    if not sep or not tenant:
        raise InvalidKeyError("not a limiter key: %r" % (key,))
    return tenant, route
