"""The :class:`Environment` holds the configuration shared by templates
(loader, filters, tests, globals, autoescaping) and caches loaded templates.
"""

from . import filters as builtin_filters
from .errors import TemplateNotFound
from .loaders import split_template_path
from .parser import Parser
from .renderer import Renderer
from .runtime import Context, Undefined
from .utils import LRUCache, is_identifier


class Environment(object):
    """Configuration and template cache.

    :param loader: a :class:`~stencil.loaders.BaseLoader`; needed for
        :meth:`get_template` and ``{% include %}``.
    :param autoescape: ``True``/``False``, or a callable receiving the
        template name (``None`` for string templates) and returning a bool,
        e.g. :func:`~stencil.escaping.select_autoescape`.
    :param undefined: the class used for missing variables
        (:class:`~stencil.runtime.Undefined` or ``StrictUndefined``).
    :param cache_size: number of loaded templates kept (0 disables caching).
    :param max_include_depth: nesting limit for ``{% include %}``.
    """

    def __init__(self, loader=None, autoescape=False, undefined=Undefined,
                 cache_size=64, max_include_depth=20, globals=None):
        self.loader = loader
        self.autoescape = autoescape
        self.undefined = undefined
        self.max_include_depth = max_include_depth
        self.filters = dict(builtin_filters.DEFAULT_FILTERS)
        self.tests = dict(builtin_filters.DEFAULT_TESTS)
        self.globals = dict(globals or {})
        self.cache = LRUCache(cache_size)

    # ---------------------------------------------------------- extension

    def add_filter(self, name, func):
        """Register ``func`` as filter ``name`` for this environment only."""
        if not is_identifier(name):
            raise ValueError("invalid filter name %r" % name)
        self.filters[name] = func

    def add_test(self, name, func):
        if not is_identifier(name):
            raise ValueError("invalid test name %r" % name)
        self.tests[name] = func

    def is_autoescape(self, name):
        """Whether templates called ``name`` are rendered with autoescaping."""
        if callable(self.autoescape):
            return bool(self.autoescape(name))
        return bool(self.autoescape)

    # ------------------------------------------------------------ loading

    def parse(self, source, name=None):
        """Parse ``source`` and return the AST (:class:`stencil.nodes.Template`)."""
        return Parser(source, name).parse()

    def from_string(self, source, name=None):
        """Compile a template from a string (it is not cached)."""
        return Template(self, self.parse(source, name), name=name, source=source)

    def get_template(self, name):
        """Load (or fetch from the cache) the template called ``name``."""
        if self.loader is None:
            raise TemplateNotFound(name, "no loader configured to load %r" % name)
        name = "/".join(split_template_path(name))
        template = self.cache.get(name)
        if template is not None:
            return template
        source, filename = self.loader.get_source(self, name)
        template = Template(self, self.parse(source, name), name=name,
                            source=source, filename=filename)
        self.cache.set(name, template)
        return template

    def list_templates(self):
        if self.loader is None:
            return []
        return self.loader.list_templates()

    def render_string(self, source, *args, **kwargs):
        """Shortcut: ``from_string(source).render(...)``."""
        return self.from_string(source).render(*args, **kwargs)


class Template(object):
    """A parsed template bound to its environment."""

    def __init__(self, environment, ast, name=None, source=None, filename=None):
        self.environment = environment
        self.ast = ast
        self.name = name
        self.source = source
        self.filename = filename
        self.autoescape = environment.is_autoescape(name)

    def new_context(self, variables=None):
        env = self.environment
        return Context(env.globals, variables, env.undefined)

    def render(self, *args, **kwargs):
        """Render with the variables of ``dict(*args, **kwargs)``."""
        variables = dict(*args, **kwargs)
        context = self.new_context(variables)
        return Renderer(self.environment, self.autoescape).render(self.ast, context)

    def __repr__(self):
        return "<Template %s>" % (repr(self.name) if self.name else "memory")
