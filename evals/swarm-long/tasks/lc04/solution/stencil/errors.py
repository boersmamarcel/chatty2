"""Exception classes raised by stencil.

Every exception derives from :class:`TemplateError`, so callers that only
want to know "the template failed" can catch that one class.

* :class:`TemplateSyntaxError` is raised while a template is lexed or
  parsed. It carries the position of the problem (``lineno``, ``col``) and
  the template ``name`` so that editors and log lines can point at it.
* :class:`TemplateRuntimeError` (and its subclass :class:`UndefinedError`)
  is raised while a parsed template is rendered.
* :class:`TemplateNotFound` is raised by loaders and by
  :meth:`stencil.environment.Environment.get_template`.
"""


class TemplateError(Exception):
    """Base class of every stencil error."""

    def __init__(self, message=None):
        Exception.__init__(self, message)
        self.message = message

    def __str__(self):
        return self.message or ""


class TemplateSyntaxError(TemplateError):
    """The template source is malformed.

    :param message: the bare description of the problem, without position.
    :param lineno: line number of the problem.
    :param col: column of the problem.
    :param name: the template name, or ``None`` for templates created with
        :meth:`~stencil.environment.Environment.from_string`.
    """

    def __init__(self, message, lineno=None, col=None, name=None):
        TemplateError.__init__(self, message)
        self.lineno = lineno
        self.col = col
        self.name = name

    def __str__(self):
        name = self.name if self.name is not None else "<string>"
        return "%s:%s:%s: %s" % (name, self.lineno, self.col, self.message)


class TemplateRuntimeError(TemplateError):
    """Rendering a template failed (bad filter call, include loop, ...)."""


class UndefinedError(TemplateRuntimeError):
    """A strict undefined value was printed, iterated or compared."""


class FilterArgumentError(TemplateRuntimeError):
    """A filter was called with arguments it cannot use."""


class TemplateNotFound(IOError, LookupError, TemplateError):
    """A loader could not find the template ``name``."""

    def __init__(self, name, message=None):
        if message is None:
            message = name
        IOError.__init__(self, message)
        self.message = message
        self.name = name

    def __str__(self):
        return self.message or ""
