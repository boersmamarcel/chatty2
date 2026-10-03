"""Split template source into text, output-tag and block-tag tokens.

The template syntax knows three kinds of tags:

* ``{{ expression }}`` prints the value of an expression (an *output* tag);
* ``{% name ... %}`` is a statement such as ``if``/``for``/``include``
  (a *block* tag);
* ``{# ... #}`` is a comment and produces no token at all.

Everything outside tags is literal text.

Whitespace control
------------------

A ``-`` directly inside a tag delimiter removes whitespace next to the tag:
``{%- ... %}`` strips the whitespace (spaces, tabs and newlines) immediately
before the tag, ``{% ... -%}`` strips the whitespace immediately after it.
See ``ISSUES.md`` for the open problems with this feature.

Every token records the offset of its first character in the source and the
matching ``lineno``/``col`` (from :func:`stencil.utils.position_of`); for
tags that is the position of the opening delimiter.
"""

import collections
import re

from .errors import TemplateSyntaxError
from .utils import position_of

TOKEN_TEXT = "text"
TOKEN_OUTPUT = "output"
TOKEN_BLOCK = "block"

#: ``value`` is the literal text for text tokens, and the tag body without
#: delimiters (and without whitespace markers) for tags. ``inner_offset`` is
#: the source offset of the first character of ``value``.
Token = collections.namedtuple("Token", "kind value offset lineno col inner_offset")

OPENER_RE = re.compile(r"\{\{|\{%|\{#")

#: opener -> (closer, token kind or None for comments, human name)
DELIMITERS = {
    "{{": ("}}", TOKEN_OUTPUT, "output tag"),
    "{%": ("%}", TOKEN_BLOCK, "block tag"),
    "{#": ("#}", None, "comment"),
}


class Lexer(object):
    """Tokenizer for one template source.

    :param source: the template text.
    :param name: the template name used in error messages (may be ``None``).
    """

    def __init__(self, source, name=None):
        self.source = source
        self.name = name

    def fail(self, message, offset):
        lineno, col = position_of(self.source, offset)
        raise TemplateSyntaxError(message, lineno, col, self.name)

    def make_token(self, kind, value, offset, inner_offset=None):
        lineno, col = position_of(self.source, offset)
        if inner_offset is None:
            inner_offset = offset
        return Token(kind, value, offset, lineno, col, inner_offset)

    def tokenize(self):
        """Return the list of tokens of the whole source."""
        source = self.source
        tokens = []
        pos = 0
        strip_next = False
        while True:
            match = OPENER_RE.search(source, pos)
            if match is None:
                text = source[pos:]
                if strip_next:
                    text = text.lstrip()
                if text:
                    tokens.append(self.make_token(TOKEN_TEXT, text, pos))
                break

            start = match.start()
            opener = match.group()
            closer, kind, label = DELIMITERS[opener]
            end = source.find(closer, start + 2)
            if end == -1:
                self.fail("unclosed %s" % label, start)

            inner = source[start + 2:end]
            inner_offset = start + 2
            lstrip_marker = False
            rstrip_marker = False
            if inner.startswith("-"):
                lstrip_marker = True
                inner = inner[1:]
                inner_offset += 1
            if inner.endswith("-"):
                rstrip_marker = True
                inner = inner[:-1]

            text = source[pos:start]
            if strip_next:
                text = text.lstrip()
            if lstrip_marker:
                text = text.rstrip()
            if text:
                tokens.append(self.make_token(TOKEN_TEXT, text, pos))

            if kind is not None:
                tokens.append(self.make_token(kind, inner, start, inner_offset))
            strip_next = rstrip_marker
            pos = end + 2
        return tokens


def tokenize(source, name=None):
    """Convenience wrapper: ``Lexer(source, name).tokenize()``."""
    return Lexer(source, name).tokenize()
