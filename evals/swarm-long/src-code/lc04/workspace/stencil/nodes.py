"""AST node classes produced by the parser and walked by the renderer.

Every node declares its ``fields``; the constructor takes them positionally
or by keyword, and ``lineno`` is accepted as an extra keyword. Nodes compare
equal when they are of the same class and all fields are equal, which keeps
parser tests short.
"""


class Node(object):
    """Base class of all nodes."""

    fields = ()

    def __init__(self, *args, **kwargs):
        if len(args) > len(self.fields):
            raise TypeError("%s takes at most %d arguments" % (type(self).__name__, len(self.fields)))
        self.lineno = kwargs.pop("lineno", None)
        values = dict(zip(self.fields, args))
        for key, value in kwargs.items():
            if key not in self.fields:
                raise TypeError("%s has no field %r" % (type(self).__name__, key))
            if key in values:
                raise TypeError("%s got field %r twice" % (type(self).__name__, key))
            values[key] = value
        for field in self.fields:
            setattr(self, field, values.get(field, self.default_for(field)))

    def default_for(self, field):
        """Default value of a field that was not given (``None``)."""
        return None

    def iter_fields(self):
        for field in self.fields:
            yield field, getattr(self, field)

    def __eq__(self, other):
        return type(self) is type(other) and tuple(self.iter_fields()) == tuple(other.iter_fields())

    def __ne__(self, other):
        return not self.__eq__(other)

    __hash__ = None

    def __repr__(self):
        args = ", ".join("%s=%r" % pair for pair in self.iter_fields())
        return "%s(%s)" % (type(self).__name__, args)


# ------------------------------------------------------------------ statements

class Template(Node):
    """The root node: ``body`` is a list of statement nodes."""
    fields = ("body",)


class Text(Node):
    """Literal template text."""
    fields = ("data",)


class Output(Node):
    """``{{ expr }}``."""
    fields = ("expr",)


class If(Node):
    """``{% if %}`` with ``branches`` = [(test, body), ...] and ``else_`` body."""
    fields = ("branches", "else_")


class For(Node):
    """``{% for targets in iter [if test] %}body{% else %}else_{% endfor %}``.

    ``targets`` is a list of names (more than one for tuple unpacking);
    ``test`` is the optional filter condition (``None`` when absent).
    """
    fields = ("targets", "iter", "body", "else_", "test")


class Set(Node):
    """``{% set name = expr %}``."""
    fields = ("name", "expr")


class With(Node):
    """``{% with a = 1, b = x %}body{% endwith %}`` - ``assignments`` is [(name, expr)]."""
    fields = ("assignments", "body")


class Include(Node):
    """``{% include expr [ignore missing] [with context|without context] %}``."""
    fields = ("template", "ignore_missing", "with_context")


class FilterBlock(Node):
    """``{% filter upper %}body{% endfilter %}`` - ``filters`` is [(name, args, kwargs)]."""
    fields = ("filters", "body")


# ----------------------------------------------------------------- expressions

class Const(Node):
    """A literal: number, string, ``true``/``false``/``none``."""
    fields = ("value",)


class Name(Node):
    """A variable lookup."""
    fields = ("name",)


class Getattr(Node):
    """``obj.attr``."""
    fields = ("node", "attr")


class Getitem(Node):
    """``obj[key]``."""
    fields = ("node", "arg")


class Call(Node):
    """``func(args..., key=value...)``; ``kwargs`` is a list of (name, expr)."""
    fields = ("node", "args", "kwargs")


class Filter(Node):
    """``node|name(args...)``."""
    fields = ("node", "name", "args", "kwargs")


class Test(Node):
    """``node is [not] name(args...)``."""
    fields = ("node", "name", "args", "negated")


class BinOp(Node):
    """Arithmetic: ``op`` is one of ``+ - * / // %``."""
    fields = ("op", "left", "right")


class Concat(Node):
    """``a ~ b ~ c`` - string concatenation of all ``nodes``."""
    fields = ("nodes",)


class Neg(Node):
    """Unary minus."""
    fields = ("node",)


class Not(Node):
    """``not node``."""
    fields = ("node",)


class And(Node):
    fields = ("left", "right")


class Or(Node):
    fields = ("left", "right")


class Compare(Node):
    """``expr op1 expr1 op2 expr2 ...`` - ``ops`` is a list of (op, expr)."""
    fields = ("expr", "ops")


class List(Node):
    """``[a, b, c]``."""
    fields = ("items",)


class Dict(Node):
    """``{key: value, ...}`` - ``items`` is a list of (key expr, value expr)."""
    fields = ("items",)


def walk(node):
    """Yield ``node`` and every node below it (depth first)."""
    yield node
    for _, value in node.iter_fields():
        for child in _children(value):
            for sub in walk(child):
                yield sub


def _children(value):
    if isinstance(value, Node):
        yield value
    elif isinstance(value, (list, tuple)):
        for item in value:
            for child in _children(item):
                yield child
