"""Template loaders: find the source of a template by name.

Template names always use ``/`` as separator, independent of the operating
system, and are relative to the loader's root (a loader never serves a file
outside its search path).

A loader implements ``get_source(environment, name)`` returning
``(source, filename)`` where ``filename`` is the file the source came from
(or ``None``), and raises :class:`~stencil.errors.TemplateNotFound` when the
template does not exist.
"""

import os

from .errors import TemplateNotFound


def split_template_path(name):
    """Split a template name into its path segments."""
    return [piece for piece in name.split("/") if piece]


class BaseLoader(object):
    """Base class; subclasses implement :meth:`get_source`."""

    def get_source(self, environment, name):
        raise TemplateNotFound(name)

    def list_templates(self):
        """Names of every template this loader knows (sorted)."""
        raise TypeError("this loader cannot list its templates")


class DictLoader(BaseLoader):
    """Serve templates from a ``{name: source}`` mapping (handy in tests)."""

    def __init__(self, mapping):
        self.mapping = mapping

    def get_source(self, environment, name):
        if name in self.mapping:
            return self.mapping[name], None
        raise TemplateNotFound(name)

    def list_templates(self):
        return sorted(self.mapping)


class FileSystemLoader(BaseLoader):
    """Load templates from one or more directories (searched in order)."""

    def __init__(self, searchpath, encoding="utf-8"):
        if isinstance(searchpath, str):
            searchpath = [searchpath]
        self.searchpath = [os.fspath(p) if hasattr(os, "fspath") else p for p in searchpath]
        self.encoding = encoding

    def get_source(self, environment, name):
        pieces = split_template_path(name)
        for root in self.searchpath:
            filename = os.path.join(root, *pieces)
            if os.path.isfile(filename):
                with open(filename, encoding=self.encoding) as f:
                    return f.read(), filename
        raise TemplateNotFound(name)

    def list_templates(self):
        found = set()
        for root in self.searchpath:
            for dirpath, dirnames, filenames in os.walk(root):
                dirnames.sort()
                for filename in filenames:
                    rel = os.path.relpath(os.path.join(dirpath, filename), root)
                    found.add(rel.replace(os.sep, "/"))
        return sorted(found)


class ChainLoader(BaseLoader):
    """Ask several loaders in turn; the first one that has the template wins."""

    def __init__(self, loaders):
        self.loaders = list(loaders)

    def get_source(self, environment, name):
        for loader in self.loaders:
            try:
                return loader.get_source(environment, name)
            except TemplateNotFound:
                continue
        raise TemplateNotFound(name)

    def list_templates(self):
        found = set()
        for loader in self.loaders:
            try:
                found.update(loader.list_templates())
            except TypeError:
                continue
        return sorted(found)
