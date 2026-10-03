"""Reference fixes for lc04 (stencil). Exact, unique text replacements."""

FIXES = {
    # Issue 1: loop variables (runtime.py)
    1: [
        ("stencil/runtime.py",
         """    def __init__(self, iterable, depth0=0, undefined=Undefined):
        self._items = iterable
        self.index0 = -1""",
         """    def __init__(self, iterable, depth0=0, undefined=Undefined):
        self._items = list(iterable)
        self.index0 = -1"""),
        ("stencil/runtime.py",
         """    @property
    def revindex(self):
        return self.length - self.index
""",
         """    @property
    def revindex(self):
        return self.length - self.index0
"""),
        ("stencil/runtime.py",
         """    @property
    def last(self):
        return self.index0 == self.length
""",
         """    @property
    def last(self):
        return self.index0 == self.length - 1

    @property
    def previtem(self):
        if self.index0 <= 0:
            return self._undefined("loop.previtem")
        return self._items[self.index0 - 1]

    @property
    def nextitem(self):
        if self.index0 + 1 >= self.length:
            return self._undefined("loop.nextitem")
        return self._items[self.index0 + 1]

    def cycle(self, *values):
        if not values:
            raise TypeError("loop.cycle() needs at least one value")
        return values[self.index0 % len(values)]
"""),
    ],

    # Issue 2: whitespace control (lexer.py)
    2: [
        ("stencil/lexer.py",
         """                text = source[pos:]
                if strip_next:
                    text = text.lstrip(" \\t")""",
         """                text = source[pos:]
                if strip_next:
                    text = text.lstrip()"""),
        ("stencil/lexer.py",
         """            if kind == TOKEN_BLOCK:
                body = inner.strip()
                if body.startswith("-"):
                    lstrip_marker = True
                    cut = inner.index("-") + 1
                    inner = inner[cut:]
                    inner_offset += cut
                if body.endswith("-"):
                    rstrip_marker = True
                    inner = inner[:inner.rindex("-")]

            text = source[pos:start]
            if strip_next:
                text = text.lstrip(" \\t")
            if lstrip_marker:
                text = text.rstrip(" \\t")""",
         """            if inner.startswith("-"):
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
                text = text.rstrip()"""),
    ],

    # Issue 3: escaping + safe/escape filters (escaping.py, filters.py)
    3: [
        ("stencil/escaping.py",
         """    def __add__(self, other):
        if isinstance(other, str):
            return Markup(str.__add__(self, other))
        return NotImplemented

    def __radd__(self, other):
        if isinstance(other, str):
            return Markup(str.__add__(other, self))
        return NotImplemented""",
         """    def __add__(self, other):
        if isinstance(other, str):
            return Markup(str.__add__(self, escape(other)))
        return NotImplemented

    def __radd__(self, other):
        if isinstance(other, str):
            return Markup(str.__add__(escape(other), self))
        return NotImplemented"""),
        ("stencil/escaping.py",
         """        return Markup(str.join(self, iterable))""",
         """        return Markup(str.join(self, (escape(item) for item in iterable)))"""),
        ("stencil/escaping.py",
         """    text = (text.replace("<", "&lt;")
                .replace(">", "&gt;")
                .replace('"', "&#34;")
                .replace("&", "&amp;"))""",
         """    text = (text.replace("&", "&amp;")
                .replace("<", "&lt;")
                .replace(">", "&gt;")
                .replace('"', "&#34;")
                .replace("'", "&#39;"))"""),
        ("stencil/filters.py",
         """    return str(escape(value))""",
         """    return escape(value)"""),
        ("stencil/filters.py",
         """    \"\"\"Mark ``value`` as safe HTML so autoescaping leaves it alone.\"\"\"
    return str(value)""",
         """    \"\"\"Mark ``value`` as safe HTML so autoescaping leaves it alone.\"\"\"
    return Markup(value)"""),
    ],

    # Issue 4: error positions and message format (utils.py, errors.py)
    4: [
        ("stencil/utils.py",
         """    prefix = source[:offset]
    lineno = prefix.count("\\n")
    col = len(prefix) - prefix.rfind("\\n")
    return lineno, col""",
         """    prefix = source[:offset].replace("\\r\\n", "\\n").replace("\\r", "\\n")
    lineno = prefix.count("\\n") + 1
    col = len(prefix) - prefix.rfind("\\n")
    return lineno, col"""),
        ("stencil/errors.py",
         """        return "%s (line %s)" % (self.message, self.lineno)""",
         """        name = self.name if self.name is not None else "<string>"
        return "%s:%s:%s: %s" % (name, self.lineno, self.col, self.message)"""),
    ],

    # Issue 5: rounding and file sizes (numbers.py)
    5: [
        ("stencil/numbers.py",
         """from decimal import ROUND_HALF_UP, Decimal, InvalidOperation""",
         """from decimal import ROUND_CEILING, ROUND_FLOOR, ROUND_HALF_UP, Decimal, InvalidOperation"""),
        ("stencil/numbers.py",
         """    value = float(value)
    precision = int(precision)
    if method == "common":
        return float(round(value, precision))
    func = math.ceil if method == "ceil" else math.floor
    factor = 10 ** precision
    return float(func(value * factor) / factor)""",
         """    rounding = {"common": ROUND_HALF_UP, "ceil": ROUND_CEILING, "floor": ROUND_FLOOR}[method]
    number = to_decimal(value).quantize(quantum(int(precision)), rounding=rounding)
    return float(number)"""),
        ("stencil/numbers.py",
         """    size = float(value)
    base = 1024 if binary else 1000
    prefixes = BINARY_PREFIXES if binary else DECIMAL_PREFIXES
    if size == 1:
        return "1 Byte"
    if size <= base:
        return "%d Bytes" % size
    for i, prefix in enumerate(prefixes):
        unit = base ** (i + 2)
        if size <= unit:
            return "%.1f %s" % (base * size / unit, prefix)
    return "%.1f %s" % (base * size / unit, prefix)""",
         """    size = to_decimal(value)
    base = 1024 if binary else 1000
    prefixes = BINARY_PREFIXES if binary else DECIMAL_PREFIXES
    if size == 1:
        return "1 Byte"
    if size < base:
        return "%d Bytes" % int(size)
    for i, prefix in enumerate(prefixes):
        unit = base ** (i + 2)
        if size < unit or i == len(prefixes) - 1:
            scaled = (size * base / unit).quantize(Decimal("0.1"), rounding=ROUND_HALF_UP)
            return "%s %s" % (scaled, prefix)"""),
    ],

    # Issue 6: template name normalisation (loaders.py, environment.py)
    6: [
        ("stencil/loaders.py",
         """    \"\"\"Split a template name into its path segments.\"\"\"
    return [piece for piece in name.split("/") if piece]""",
         """    \"\"\"Split a template name into its path segments.\"\"\"
    pieces = []
    for piece in name.replace("\\\\", "/").split("/"):
        if piece == "..":
            raise TemplateNotFound(name)
        if piece and piece != ".":
            pieces.append(piece)
    if not pieces:
        raise TemplateNotFound(name)
    return pieces"""),
        ("stencil/loaders.py",
         """        if name in self.mapping:
            return self.mapping[name], None
        raise TemplateNotFound(name)""",
         """        key = "/".join(split_template_path(name))
        if key in self.mapping:
            return self.mapping[key], None
        raise TemplateNotFound(name)"""),
        ("stencil/environment.py",
         """from .errors import TemplateNotFound
""",
         """from .errors import TemplateNotFound
from .loaders import split_template_path
"""),
        ("stencil/environment.py",
         """            raise TemplateNotFound(name, "no loader configured to load %r" % name)
        template = self.cache.get(name)""",
         """            raise TemplateNotFound(name, "no loader configured to load %r" % name)
        name = "/".join(split_template_path(name))
        template = self.cache.get(name)"""),
    ],

    # Issue 7: {% for ... if cond %} loop filtering (parser.py, renderer.py)
    7: [
        ("stencil/parser.py",
         """        iter_ = self.expression(stream, token)
        self.end_of_tag(stream, token)
        body, end, tag = self.subparse(("else", "endfor"))""",
         """        iter_ = self.expression(stream, token)
        test = None
        if stream.skip_if("name", "if"):
            test = self.expression(stream, token)
        self.end_of_tag(stream, token)
        body, end, tag = self.subparse(("else", "endfor"))"""),
        ("stencil/parser.py",
         """        return nodes.For(targets, iter_, body, else_, lineno=token.lineno)""",
         """        return nodes.For(targets, iter_, body, else_, test, lineno=token.lineno)"""),
        ("stencil/renderer.py",
         """        if isinstance(iterable, dict):
            iterable = list(iterable)
        outer = context.resolve("loop")""",
         """        if isinstance(iterable, dict):
            iterable = list(iterable)
        if node.test is not None:
            iterable = [item for item in iterable if self.passes(node, item, context)]
        outer = context.resolve("loop")"""),
        ("stencil/renderer.py",
         """    def visit_Include(self, node, context, out):""",
         """    def passes(self, node, item, context):
        \"\"\"Evaluate the ``if`` condition of a for loop for one item.\"\"\"
        context.push(self.assign_targets(node.targets, item))
        try:
            return bool(self.evaluate(node.test, context))
        finally:
            context.pop()

    def visit_Include(self, node, context, out):"""),
    ],
}
