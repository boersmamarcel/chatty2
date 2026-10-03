"""Runtime objects used while rendering: undefined values, the variable
context and the ``loop`` helper of ``{% for %}``.
"""

from .errors import UndefinedError


class Undefined(object):
    """The value of a variable or attribute that does not exist.

    It prints as an empty string, is falsy, iterates as an empty sequence and
    has length 0, so templates can test and loop over optional values
    without errors. Attribute and item access on it return another
    :class:`Undefined`.
    """

    __slots__ = ("_name",)

    def __init__(self, name=None):
        self._name = name

    def _fail(self, *args, **kwargs):
        if self._name is None:
            raise UndefinedError("value is undefined")
        raise UndefinedError("'%s' is undefined" % self._name)

    def __getattr__(self, name):
        if name.startswith("__"):
            raise AttributeError(name)
        return type(self)(self._name and "%s.%s" % (self._name, name))

    def __getitem__(self, key):
        return type(self)(self._name and "%s[%r]" % (self._name, key))

    def __str__(self):
        return ""

    def __iter__(self):
        return iter(())

    def __bool__(self):
        return False

    def __len__(self):
        return 0

    def __eq__(self, other):
        return type(self) is type(other)

    def __ne__(self, other):
        return not self.__eq__(other)

    def __hash__(self):
        return id(type(self))

    def __repr__(self):
        return "Undefined(%r)" % (self._name,)

    __call__ = _fail
    __add__ = __radd__ = __sub__ = __rsub__ = _fail
    __mul__ = __rmul__ = __truediv__ = __rtruediv__ = _fail
    __lt__ = __le__ = __gt__ = __ge__ = _fail


class StrictUndefined(Undefined):
    """An undefined value that raises :class:`UndefinedError` on any use
    other than an ``is defined`` test."""

    __slots__ = ()

    __str__ = __iter__ = __len__ = Undefined._fail
    __eq__ = __ne__ = Undefined._fail

    def __bool__(self):
        self._fail()

    def __hash__(self):
        return id(type(self))


def is_undefined(value):
    return isinstance(value, Undefined)


class Context(object):
    """The variables visible to a template, as a stack of scopes.

    The bottom scope holds the environment globals, the next one the render
    arguments; ``{% for %}``, ``{% with %}`` and includes push further
    scopes. Lookups search from the top down.
    """

    def __init__(self, globals_=None, variables=None, undefined=Undefined):
        self.scopes = [dict(globals_ or {}), dict(variables or {})]
        self.undefined = undefined

    def resolve(self, name):
        for scope in reversed(self.scopes):
            if name in scope:
                return scope[name]
        return self.undefined(name)

    def __contains__(self, name):
        return any(name in scope for scope in self.scopes)

    def push(self, mapping=None):
        self.scopes.append(dict(mapping or {}))

    def pop(self):
        if len(self.scopes) <= 2:
            raise RuntimeError("cannot pop the root scopes of a context")
        self.scopes.pop()

    def set(self, name, value):
        """Assign in the innermost scope (``{% set %}``)."""
        self.scopes[-1][name] = value

    def get_all(self):
        """All visible variables merged into one dict."""
        merged = {}
        for scope in self.scopes:
            merged.update(scope)
        return merged

    def derived(self):
        """A fresh context with the same globals and render arguments only."""
        new = Context(self.scopes[0], self.scopes[1], self.undefined)
        return new


class LoopContext(object):
    """The ``loop`` variable inside ``{% for %}``.

    Iterating over the loop context yields the items of the underlying
    iterable and keeps the counters up to date. Attributes:

    ``index`` / ``index0``
        the current iteration, counted from 1 / from 0;
    ``revindex`` / ``revindex0``
        iterations left until the end, counted down to 1 / to 0;
    ``first`` / ``last``
        true on the first / last iteration;
    ``length``
        the number of items;
    ``depth`` / ``depth0``
        nesting level of the loop, starting at 1 / 0 for the outermost loop.
    """

    def __init__(self, iterable, depth0=0, undefined=Undefined):
        self._items = iterable
        self.index0 = -1
        self.depth0 = depth0
        self._undefined = undefined

    def __iter__(self):
        for item in self._items:
            self.index0 += 1
            yield item

    @property
    def length(self):
        return len(self._items)

    @property
    def index(self):
        return self.index0 + 1

    @property
    def revindex(self):
        return self.length - self.index

    @property
    def revindex0(self):
        return self.length - self.index

    @property
    def first(self):
        return self.index0 == 0

    @property
    def last(self):
        return self.index0 == self.length

    @property
    def depth(self):
        return self.depth0 + 1

    def __len__(self):
        return self.length

    def __repr__(self):
        return "<LoopContext %d/%d>" % (self.index, self.length)
