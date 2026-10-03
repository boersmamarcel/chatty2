"""Tokenizer and recursive-descent parser for tag expressions.

Grammar, from the loosest to the tightest binding::

    or_expr   := and_expr ("or" and_expr)*
    and_expr  := not_expr ("and" not_expr)*
    not_expr  := "not" not_expr | compare
    compare   := concat (cmp_op concat | "is" ["not"] NAME [args])*
    cmp_op    := "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" | "not" "in"
    concat    := sum ("~" sum)*
    sum       := product (("+" | "-") product)*
    product   := unary (("*" | "/" | "//" | "%") unary)*
    unary     := "-" unary | filtered
    filtered  := postfix ("|" NAME [args])*
    postfix   := primary ("." NAME | "[" or_expr "]" | args)*
    primary   := NUMBER | STRING | NAME | "(" or_expr ")" | list | dict

The parser stops at the first token that cannot continue the expression and
leaves it in the stream; statement parsers decide whether that is an error.
"""

import collections
import re

from . import nodes

ExprToken = collections.namedtuple("ExprToken", "type value")

TOKEN_RE = re.compile(r"""
    (?P<ws>\s+)
  | (?P<float>\d+\.\d+)
  | (?P<int>\d+)
  | (?P<string>'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*")
  | (?P<name>[A-Za-z_][A-Za-z0-9_]*)
  | (?P<op>//|==|!=|<=|>=|[-+*/%~|.,()\[\]{}:<>=])
""", re.X | re.S)

ESCAPES = {"n": "\n", "t": "\t", "r": "\r", "\\": "\\", "'": "'", '"': '"'}

CONSTANTS = {
    "true": True, "True": True,
    "false": False, "False": False,
    "none": None, "None": None,
}

COMPARE_OPS = ("==", "!=", "<", "<=", ">", ">=")


class ExpressionError(ValueError):
    """Raised by the tokenizer/parser; the caller attaches the position."""


def unquote(literal):
    """Decode a quoted string literal (``'a\\'b'`` -> ``a'b``)."""
    body = literal[1:-1]
    out = []
    i = 0
    while i < len(body):
        ch = body[i]
        if ch == "\\" and i + 1 < len(body):
            nxt = body[i + 1]
            out.append(ESCAPES.get(nxt, "\\" + nxt))
            i += 2
            continue
        out.append(ch)
        i += 1
    return "".join(out)


def tokenize(text):
    """Return the list of :class:`ExprToken` of ``text`` ending with ``eof``."""
    tokens = []
    pos = 0
    while pos < len(text):
        match = TOKEN_RE.match(text, pos)
        if match is None:
            raise ExpressionError("unexpected character %r" % text[pos])
        kind = match.lastgroup
        value = match.group()
        pos = match.end()
        if kind == "ws":
            continue
        if kind == "int":
            tokens.append(ExprToken("number", int(value)))
        elif kind == "float":
            tokens.append(ExprToken("number", float(value)))
        elif kind == "string":
            tokens.append(ExprToken("string", unquote(value)))
        else:
            tokens.append(ExprToken(kind, value))
    tokens.append(ExprToken("eof", None))
    return tokens


class TokenStream(object):
    """A cursor over expression tokens."""

    def __init__(self, tokens):
        self.tokens = tokens
        self.pos = 0

    @property
    def current(self):
        return self.tokens[self.pos]

    def look(self, distance=1):
        index = min(self.pos + distance, len(self.tokens) - 1)
        return self.tokens[index]

    def next(self):
        token = self.current
        if token.type != "eof":
            self.pos += 1
        return token

    def test(self, type_, value=None):
        token = self.current
        return token.type == type_ and (value is None or token.value == value)

    def skip_if(self, type_, value=None):
        if self.test(type_, value):
            return self.next()
        return None

    def expect(self, type_, value=None):
        if not self.test(type_, value):
            wanted = value if value is not None else type_
            raise ExpressionError("expected %r, got %s" % (wanted, describe(self.current)))
        return self.next()

    @property
    def at_end(self):
        return self.current.type == "eof"


def describe(token):
    """Human description of a token for error messages."""
    if token.type == "eof":
        return "end of tag"
    return repr(token.value)


class ExpressionParser(object):
    """Parse expressions from a :class:`TokenStream`."""

    def __init__(self, stream):
        self.stream = stream

    def parse_expression(self):
        return self.parse_or()

    def parse_or(self):
        left = self.parse_and()
        while self.stream.skip_if("name", "or"):
            left = nodes.Or(left, self.parse_and())
        return left

    def parse_and(self):
        left = self.parse_not()
        while self.stream.skip_if("name", "and"):
            left = nodes.And(left, self.parse_not())
        return left

    def parse_not(self):
        if self.stream.test("name", "not") and not self.stream.look().value == "in":
            self.stream.next()
            return nodes.Not(self.parse_not())
        return self.parse_compare()

    def parse_compare(self):
        expr = self.parse_concat()
        ops = []
        stream = self.stream
        while True:
            token = stream.current
            if token.type == "op" and token.value in COMPARE_OPS:
                stream.next()
                ops.append((token.value, self.parse_concat()))
            elif stream.test("name", "in"):
                stream.next()
                ops.append(("in", self.parse_concat()))
            elif stream.test("name", "not") and stream.look().type == "name" and stream.look().value == "in":
                stream.next()
                stream.next()
                ops.append(("notin", self.parse_concat()))
            elif stream.test("name", "is"):
                stream.next()
                if ops:
                    expr = nodes.Compare(expr, ops)
                    ops = []
                expr = self.parse_test(expr)
            else:
                break
        if ops:
            return nodes.Compare(expr, ops)
        return expr

    def parse_test(self, node):
        negated = bool(self.stream.skip_if("name", "not"))
        name = self.stream.expect("name").value
        args = []
        if self.stream.test("op", "("):
            args, kwargs = self.parse_call_args()
            if kwargs:
                raise ExpressionError("tests take no keyword arguments")
        return nodes.Test(node, name, args, negated)

    def parse_concat(self):
        items = [self.parse_sum()]
        while self.stream.skip_if("op", "~"):
            items.append(self.parse_sum())
        if len(items) == 1:
            return items[0]
        return nodes.Concat(items)

    def parse_sum(self):
        left = self.parse_product()
        while self.stream.current.type == "op" and self.stream.current.value in ("+", "-"):
            op = self.stream.next().value
            left = nodes.BinOp(op, left, self.parse_product())
        return left

    def parse_product(self):
        left = self.parse_unary()
        while self.stream.current.type == "op" and self.stream.current.value in ("*", "/", "//", "%"):
            op = self.stream.next().value
            left = nodes.BinOp(op, left, self.parse_unary())
        return left

    def parse_unary(self):
        if self.stream.skip_if("op", "-"):
            return nodes.Neg(self.parse_unary())
        if self.stream.skip_if("op", "+"):
            return self.parse_unary()
        return self.parse_filtered()

    def parse_filtered(self):
        node = self.parse_postfix()
        while self.stream.skip_if("op", "|"):
            node = self.parse_filter(node)
        return node

    def parse_filter(self, node):
        name = self.stream.expect("name").value
        args, kwargs = [], []
        if self.stream.test("op", "("):
            args, kwargs = self.parse_call_args()
        return nodes.Filter(node, name, args, kwargs)

    def parse_postfix(self):
        node = self.parse_primary()
        stream = self.stream
        while True:
            if stream.skip_if("op", "."):
                token = stream.current
                if token.type == "number" and isinstance(token.value, int):
                    stream.next()
                    node = nodes.Getitem(node, nodes.Const(token.value))
                else:
                    node = nodes.Getattr(node, stream.expect("name").value)
            elif stream.skip_if("op", "["):
                arg = self.parse_expression()
                stream.expect("op", "]")
                node = nodes.Getitem(node, arg)
            elif stream.test("op", "("):
                args, kwargs = self.parse_call_args()
                node = nodes.Call(node, args, kwargs)
            else:
                return node

    def parse_call_args(self):
        """Parse ``( arg, ..., name=value, ... )``; positional args come first."""
        stream = self.stream
        stream.expect("op", "(")
        args, kwargs = [], []
        while not stream.test("op", ")"):
            if args or kwargs:
                stream.expect("op", ",")
                if stream.test("op", ")"):
                    break
            if stream.current.type == "name" and stream.look().type == "op" and stream.look().value == "=":
                key = stream.next().value
                stream.next()
                kwargs.append((key, self.parse_expression()))
            else:
                if kwargs:
                    raise ExpressionError("positional argument after keyword argument")
                args.append(self.parse_expression())
        stream.expect("op", ")")
        return args, kwargs

    def parse_primary(self):
        stream = self.stream
        token = stream.current
        if token.type in ("number", "string"):
            stream.next()
            node = nodes.Const(token.value)
            if token.type == "string":
                while stream.current.type == "string":
                    node = nodes.Const(node.value + stream.next().value)
            return node
        if token.type == "name":
            stream.next()
            if token.value in CONSTANTS:
                return nodes.Const(CONSTANTS[token.value])
            return nodes.Name(token.value)
        if stream.skip_if("op", "("):
            node = self.parse_expression()
            stream.expect("op", ")")
            return node
        if stream.skip_if("op", "["):
            items = []
            while not stream.test("op", "]"):
                if items:
                    stream.expect("op", ",")
                    if stream.test("op", "]"):
                        break
                items.append(self.parse_expression())
            stream.expect("op", "]")
            return nodes.List(items)
        if stream.skip_if("op", "{"):
            items = []
            while not stream.test("op", "}"):
                if items:
                    stream.expect("op", ",")
                    if stream.test("op", "}"):
                        break
                key = self.parse_expression()
                stream.expect("op", ":")
                items.append((key, self.parse_expression()))
            stream.expect("op", "}")
            return nodes.Dict(items)
        if token.type == "eof":
            raise ExpressionError("unexpected end of expression")
        raise ExpressionError("unexpected %s" % describe(token))


def parse(text):
    """Parse a complete expression string (the whole text must be consumed)."""
    stream = TokenStream(tokenize(text))
    node = ExpressionParser(stream).parse_expression()
    if not stream.at_end:
        raise ExpressionError("unexpected %s" % describe(stream.current))
    return node
