"""stencil: a small text templating engine.

The engine is split into the classic stages:

* :mod:`stencil.lexer` cuts the template source into text, output tags
  (``{{ ... }}``), block tags (``{% ... %}``) and comments (``{# ... #}``);
* :mod:`stencil.expressions` tokenizes and parses the expressions inside tags;
* :mod:`stencil.parser` builds the template AST (:mod:`stencil.nodes`);
* :mod:`stencil.renderer` walks the AST against a :class:`~stencil.runtime.Context`;
* :mod:`stencil.filters`, :mod:`stencil.numbers` and :mod:`stencil.escaping`
  provide the built-in filters, tests and HTML escaping;
* :mod:`stencil.loaders` finds templates by name for ``{% include %}``;
* :mod:`stencil.environment` ties everything together.

Typical use::

    from stencil import Environment, DictLoader
    env = Environment(loader=DictLoader({"hello.txt": "Hello {{ name }}!"}))
    env.get_template("hello.txt").render(name="World")
"""

from .environment import Environment, Template
from .errors import (
    TemplateError,
    TemplateNotFound,
    TemplateRuntimeError,
    TemplateSyntaxError,
    UndefinedError,
)
from .escaping import Markup, escape, select_autoescape
from .loaders import BaseLoader, ChainLoader, DictLoader, FileSystemLoader
from .runtime import StrictUndefined, Undefined

__version__ = "0.4.1"

__all__ = [
    "BaseLoader",
    "ChainLoader",
    "DictLoader",
    "Environment",
    "FileSystemLoader",
    "Markup",
    "StrictUndefined",
    "Template",
    "TemplateError",
    "TemplateNotFound",
    "TemplateRuntimeError",
    "TemplateSyntaxError",
    "Undefined",
    "UndefinedError",
    "escape",
    "select_autoescape",
]
