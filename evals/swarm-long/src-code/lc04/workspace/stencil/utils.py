"""Small helpers shared by the lexer, the parser and the environment."""

import re
from collections import OrderedDict

IDENTIFIER_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_]*\Z")


class _Missing(object):
    """Sentinel for "no value given" where ``None`` is a valid value."""

    def __repr__(self):
        return "missing"

    def __bool__(self):
        return False


missing = _Missing()


def position_of(source, offset):
    """Return ``(lineno, col)`` of character ``offset`` in ``source``.

    Used by the lexer and the parser to attach a position to every token, so
    that :class:`~stencil.errors.TemplateSyntaxError` can point at the place
    where a problem starts.
    """
    prefix = source[:offset]
    lineno = prefix.count("\n")
    col = len(prefix) - prefix.rfind("\n")
    return lineno, col


def is_identifier(name):
    """True when ``name`` is a valid variable / filter name."""
    return bool(IDENTIFIER_RE.match(name))


def describe_value(value, limit=40):
    """A short ``repr`` of ``value`` for error messages."""
    text = repr(value)
    if len(text) > limit:
        text = text[:limit - 3] + "..."
    return text


def to_text(value):
    """Convert ``value`` to ``str`` the way the renderer prints it.

    ``None`` prints as an empty string; strings (including
    :class:`~stencil.escaping.Markup`) are returned unchanged.
    """
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return str(value)


def concat(parts):
    """Join rendered output parts."""
    return "".join(parts)


class LRUCache(object):
    """A tiny least-recently-used mapping used as the template cache.

    ``capacity`` of 0 disables caching; ``None`` means unbounded.
    """

    def __init__(self, capacity=64):
        self.capacity = capacity
        self._data = OrderedDict()

    def get(self, key, default=None):
        try:
            value = self._data.pop(key)
        except KeyError:
            return default
        self._data[key] = value
        return value

    def set(self, key, value):
        if self.capacity == 0:
            return
        self._data.pop(key, None)
        self._data[key] = value
        if self.capacity is not None:
            while len(self._data) > self.capacity:
                self._data.popitem(last=False)

    def __contains__(self, key):
        return key in self._data

    def __len__(self):
        return len(self._data)

    def keys(self):
        """Keys from least to most recently used."""
        return list(self._data.keys())

    def clear(self):
        self._data.clear()
