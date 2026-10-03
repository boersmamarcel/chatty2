"""Build the template AST from lexer tokens.

Supported statements::

    {% if expr %} ... {% elif expr %} ... {% else %} ... {% endif %}
    {% for name[, name...] in expr %} ... {% else %} ... {% endfor %}
    {% set name = expr %}
    {% with name = expr[, name = expr...] %} ... {% endwith %}
    {% include expr [ignore missing] [with context | without context] %}
    {% filter name[(args)][|name...] %} ... {% endfilter %}

Error positions: every syntax error found inside a tag is reported at the
position of the tag's opening delimiter (``{{`` or ``{%``). A block that is
never closed is reported at the tag that opened it, an unexpected end tag at
that end tag.
"""

from . import nodes
from .errors import TemplateSyntaxError
from .expressions import ExpressionError, ExpressionParser, TokenStream, describe, tokenize
from .lexer import TOKEN_BLOCK, TOKEN_OUTPUT, TOKEN_TEXT, Lexer

#: block tags that close or continue another block
END_TAGS = ("endif", "endfor", "endwith", "endfilter", "elif", "else")


class Parser(object):
    """Parse one template.

    :param source: template text.
    :param name: template name for error messages.
    """

    def __init__(self, source, name=None):
        self.source = source
        self.name = name
        self.tokens = Lexer(source, name).tokenize()
        self.pos = 0

    # -------------------------------------------------------------- helpers

    def fail(self, message, token):
        raise TemplateSyntaxError(message, token.lineno, token.col, self.name)

    def stream_for(self, token):
        """A :class:`TokenStream` over the expression tokens of a tag."""
        try:
            return TokenStream(tokenize(token.value))
        except ExpressionError as exc:
            self.fail(str(exc), token)

    def tag_name(self, token):
        words = token.value.split()
        if not words:
            self.fail("empty block tag", token)
        return words[0]

    def expression(self, stream, token):
        try:
            return ExpressionParser(stream).parse_expression()
        except ExpressionError as exc:
            self.fail(str(exc), token)

    def expect(self, stream, token, type_, value=None):
        try:
            return stream.expect(type_, value)
        except ExpressionError as exc:
            self.fail(str(exc), token)

    def end_of_tag(self, stream, token):
        if not stream.at_end:
            self.fail("expected end of tag, got %s" % describe(stream.current), token)

    def bare_tag(self, token, tag):
        """Check that a tag such as ``{% else %}`` has no arguments."""
        stream = self.stream_for(token)
        self.expect(stream, token, "name", tag)
        self.end_of_tag(stream, token)

    def unclosed(self, tag, token):
        self.fail("unclosed '%s' block" % tag, token)

    # ---------------------------------------------------------------- driver

    def parse(self):
        body = self.subparse(())[0]
        return nodes.Template(body, lineno=1)

    def subparse(self, end_tags):
        """Parse statements until one of ``end_tags`` (not consumed).

        Returns ``(body, end_token, end_tag)``; ``end_token`` is ``None`` when
        the source ran out first.
        """
        body = []
        while self.pos < len(self.tokens):
            token = self.tokens[self.pos]
            if token.kind == TOKEN_TEXT:
                body.append(nodes.Text(token.value, lineno=token.lineno))
                self.pos += 1
            elif token.kind == TOKEN_OUTPUT:
                stream = self.stream_for(token)
                expr = self.expression(stream, token)
                self.end_of_tag(stream, token)
                body.append(nodes.Output(expr, lineno=token.lineno))
                self.pos += 1
            else:
                tag = self.tag_name(token)
                if tag in end_tags:
                    return body, token, tag
                if tag in END_TAGS:
                    self.fail("unexpected '%s'" % tag, token)
                handler = getattr(self, "parse_" + tag, None)
                if handler is None:
                    self.fail("unknown tag '%s'" % tag, token)
                self.pos += 1
                body.append(handler(token))
        return body, None, None

    # ------------------------------------------------------------ statements

    def parse_if(self, token):
        stream = self.stream_for(token)
        self.expect(stream, token, "name", "if")
        test = self.expression(stream, token)
        self.end_of_tag(stream, token)
        branches = []
        else_ = []
        while True:
            body, end, tag = self.subparse(("elif", "else", "endif"))
            if end is None:
                self.unclosed("if", token)
            branches.append((test, body))
            self.pos += 1
            if tag == "elif":
                stream = self.stream_for(end)
                self.expect(stream, end, "name", "elif")
                test = self.expression(stream, end)
                self.end_of_tag(stream, end)
                continue
            if tag == "else":
                self.bare_tag(end, "else")
                else_, end, tag = self.subparse(("endif",))
                if end is None:
                    self.unclosed("if", token)
                self.pos += 1
            self.bare_tag(end, "endif")
            return nodes.If(branches, else_, lineno=token.lineno)

    def parse_for(self, token):
        stream = self.stream_for(token)
        self.expect(stream, token, "name", "for")
        targets = [self.expect(stream, token, "name").value]
        while stream.skip_if("op", ","):
            targets.append(self.expect(stream, token, "name").value)
        self.expect(stream, token, "name", "in")
        iter_ = self.expression(stream, token)
        self.end_of_tag(stream, token)
        body, end, tag = self.subparse(("else", "endfor"))
        if end is None:
            self.unclosed("for", token)
        self.pos += 1
        else_ = []
        if tag == "else":
            self.bare_tag(end, "else")
            else_, end, tag = self.subparse(("endfor",))
            if end is None:
                self.unclosed("for", token)
            self.pos += 1
        self.bare_tag(end, "endfor")
        return nodes.For(targets, iter_, body, else_, lineno=token.lineno)

    def parse_set(self, token):
        stream = self.stream_for(token)
        self.expect(stream, token, "name", "set")
        name = self.expect(stream, token, "name").value
        self.expect(stream, token, "op", "=")
        expr = self.expression(stream, token)
        self.end_of_tag(stream, token)
        return nodes.Set(name, expr, lineno=token.lineno)

    def parse_with(self, token):
        stream = self.stream_for(token)
        self.expect(stream, token, "name", "with")
        assignments = []
        while not stream.at_end:
            if assignments:
                self.expect(stream, token, "op", ",")
            name = self.expect(stream, token, "name").value
            self.expect(stream, token, "op", "=")
            assignments.append((name, self.expression(stream, token)))
        body, end, tag = self.subparse(("endwith",))
        if end is None:
            self.unclosed("with", token)
        self.pos += 1
        self.bare_tag(end, "endwith")
        return nodes.With(assignments, body, lineno=token.lineno)

    def parse_include(self, token):
        stream = self.stream_for(token)
        self.expect(stream, token, "name", "include")
        template = self.expression(stream, token)
        ignore_missing = False
        with_context = True
        if stream.test("name", "ignore") and stream.look().value == "missing":
            stream.next()
            stream.next()
            ignore_missing = True
        if stream.test("name", "with") or stream.test("name", "without"):
            with_context = stream.next().value == "with"
            self.expect(stream, token, "name", "context")
        self.end_of_tag(stream, token)
        return nodes.Include(template, ignore_missing, with_context, lineno=token.lineno)

    def parse_filter(self, token):
        stream = self.stream_for(token)
        self.expect(stream, token, "name", "filter")
        filters = []
        parser = ExpressionParser(stream)
        while True:
            name = self.expect(stream, token, "name").value
            args, kwargs = [], []
            if stream.test("op", "("):
                try:
                    args, kwargs = parser.parse_call_args()
                except ExpressionError as exc:
                    self.fail(str(exc), token)
            filters.append((name, args, kwargs))
            if not stream.skip_if("op", "|"):
                break
        self.end_of_tag(stream, token)
        body, end, tag = self.subparse(("endfilter",))
        if end is None:
            self.unclosed("filter", token)
        self.pos += 1
        self.bare_tag(end, "endfilter")
        return nodes.FilterBlock(filters, body, lineno=token.lineno)


def parse(source, name=None):
    """Parse ``source`` into a :class:`stencil.nodes.Template`."""
    return Parser(source, name).parse()
