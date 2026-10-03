"""HTML escaping and the :class:`Markup` "already safe" string type.

When autoescaping is on, the renderer passes every printed value through
:func:`escape`. Values that are already safe HTML are wrapped in
:class:`Markup` (or define ``__html__``) and are printed unchanged.
"""

import os


class Markup(str):
    """A string that is safe to insert into HTML without escaping."""

    __slots__ = ()

    def __new__(cls, value="", *args):
        if hasattr(value, "__html__") and not isinstance(value, Markup):
            value = value.__html__()
        return str.__new__(cls, value, *args)

    def __html__(self):
        return self

    def __add__(self, other):
        if isinstance(other, str):
            return Markup(str.__add__(self, escape(other)))
        return NotImplemented

    def __radd__(self, other):
        if isinstance(other, str):
            return Markup(str.__add__(escape(other), self))
        return NotImplemented

    def __mul__(self, count):
        return Markup(str.__mul__(self, count))

    def join(self, iterable):
        return Markup(str.join(self, (escape(item) for item in iterable)))

    def __repr__(self):
        return "Markup(%s)" % str.__repr__(self)

    def striptags(self):
        """The text with tags removed and whitespace collapsed."""
        out = []
        inside = False
        for ch in self:
            if ch == "<":
                inside = True
            elif ch == ">" and inside:
                inside = False
            elif not inside:
                out.append(ch)
        return " ".join("".join(out).split())


def escape(value):
    """Return ``value`` as :class:`Markup` with HTML special characters escaped.

    Values with an ``__html__`` method are trusted and only converted.
    """
    if hasattr(value, "__html__"):
        return Markup(value.__html__())
    text = str(value)
    text = (text.replace("&", "&amp;")
                .replace("<", "&lt;")
                .replace(">", "&gt;")
                .replace('"', "&#34;")
                .replace("'", "&#39;"))
    return Markup(text)


def soft_escape(value):
    """Escape ``value`` unless it is ``None`` (which becomes ``""``)."""
    if value is None:
        return Markup("")
    return escape(value)


def select_autoescape(enabled_extensions=("html", "htm", "xml"),
                      disabled_extensions=(), default_for_string=False, default=False):
    """Build an ``autoescape`` callable for :class:`~stencil.environment.Environment`.

    The callable receives the template name (``None`` for templates made
    from a string) and decides by file extension, case-insensitively.
    """
    enabled = tuple("." + ext.lstrip(".").lower() for ext in enabled_extensions)
    disabled = tuple("." + ext.lstrip(".").lower() for ext in disabled_extensions)

    def autoescape(template_name):
        if template_name is None:
            return default_for_string
        ext = os.path.splitext(template_name)[1].lower()
        if ext in enabled:
            return True
        if ext in disabled:
            return False
        return default

    return autoescape
