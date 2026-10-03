"""Built-in filters and tests.

A filter is a plain function ``f(value, *args, **kwargs)``; ``{{ x|f(1) }}``
calls ``f(x, 1)``. A test is a function ``t(value, *args)`` returning a bool
and is used as ``{% if x is t %}``.

:data:`DEFAULT_FILTERS` and :data:`DEFAULT_TESTS` are copied into every
:class:`~stencil.environment.Environment`; ``Environment.add_filter`` adds
more.
"""

from . import numbers
from .errors import FilterArgumentError
from .escaping import Markup, escape
from .runtime import Undefined
from .utils import to_text

DEFAULT_FILTERS = {}
DEFAULT_TESTS = {}


def register(*names):
    """Register the decorated function as filter under every name in ``names``."""
    def decorator(func):
        for name in names:
            DEFAULT_FILTERS[name] = func
        return func
    return decorator


def register_test(*names):
    def decorator(func):
        for name in names:
            DEFAULT_TESTS[name] = func
        return func
    return decorator


def _attr_getter(attribute):
    """Return a key function reading ``attribute`` (dotted, or an int index)."""
    if attribute is None:
        return lambda item: item
    parts = [int(p) if p.isdigit() else p for p in str(attribute).split(".")]

    def getter(item):
        for part in parts:
            try:
                item = item[part]
            except (KeyError, IndexError, TypeError):
                item = getattr(item, part, None)
        return item
    return getter


def _sort_key(case_sensitive, getter):
    def key(item):
        value = getter(item)
        if not case_sensitive and isinstance(value, str):
            return value.lower()
        return value
    return key


# ---------------------------------------------------------------- strings

@register("upper")
def do_upper(value):
    return to_text(value).upper()


@register("lower")
def do_lower(value):
    return to_text(value).lower()


@register("capitalize")
def do_capitalize(value):
    return to_text(value).capitalize()


@register("title")
def do_title(value):
    """Title-case every whitespace separated word."""
    return " ".join(word[:1].upper() + word[1:].lower() for word in to_text(value).split(" "))


@register("trim")
def do_trim(value, chars=None):
    return to_text(value).strip(chars)


@register("replace")
def do_replace(value, old, new, count=None):
    if count is None:
        return to_text(value).replace(old, new)
    return to_text(value).replace(old, new, int(count))


@register("center")
def do_center(value, width=80):
    return to_text(value).center(int(width))


@register("indent")
def do_indent(value, width=4, first=False, blank=False):
    """Indent every line but the first (unless ``first``); blank lines only
    when ``blank`` is true."""
    pad = " " * int(width) if not isinstance(width, str) else width
    lines = to_text(value).split("\n")
    out = []
    for i, line in enumerate(lines):
        if i == 0 and not first:
            out.append(line)
        elif not line.strip() and not blank:
            out.append(line)
        else:
            out.append(pad + line)
    return "\n".join(out)


@register("truncate")
def do_truncate(value, length=255, killwords=False, end="...", leeway=0):
    """Shorten text to ``length`` characters including ``end``.

    Text up to ``length + leeway`` characters is returned unchanged. Unless
    ``killwords`` is true the cut happens at the last space.
    """
    text = to_text(value)
    length = int(length)
    if len(end) > length:
        raise FilterArgumentError("truncate: length must be at least len(end)")
    if len(text) <= length + int(leeway):
        return text
    if killwords:
        return text[:length - len(end)] + end
    cut = text[:length - len(end)].rsplit(" ", 1)[0]
    return cut + end


@register("wordcount")
def do_wordcount(value):
    return len(to_text(value).split())


@register("format")
def do_format(value, *args, **kwargs):
    """printf-style: ``"%s-%d"|format("a", 3)``."""
    if args and kwargs:
        raise FilterArgumentError("format: use positional or keyword arguments, not both")
    return to_text(value) % (kwargs or args)


@register("string")
def do_string(value):
    return to_text(value)


@register("escape", "e")
def do_escape(value):
    """Escape HTML special characters; values that are already
    :class:`Markup` are not escaped twice."""
    return escape(value)


@register("safe")
def do_safe(value):
    """Mark ``value`` as safe HTML so autoescaping leaves it alone."""
    return Markup(value)


@register("striptags")
def do_striptags(value):
    return Markup(to_text(value)).striptags()


# ---------------------------------------------------------------- numbers

@register("int")
def do_int(value, default=0, base=10):
    try:
        if isinstance(value, str):
            return int(value, int(base))
        return int(value)
    except (TypeError, ValueError):
        try:
            return int(float(value))
        except (TypeError, ValueError):
            return default


@register("float")
def do_float(value, default=0.0):
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


@register("abs")
def do_abs(value):
    return abs(value)


@register("round")
def do_round(value, precision=0, method="common"):
    """Round a number; see :func:`stencil.numbers.round_value`."""
    return numbers.round_value(value, precision, method)


@register("filesizeformat")
def do_filesizeformat(value, binary=False):
    return numbers.filesizeformat(value, binary)


@register("numberformat")
def do_numberformat(value, places=2, thousands=",", point="."):
    return numbers.format_number(value, places, thousands, point)


@register("percent")
def do_percent(value, places=0):
    return numbers.percent(value, places)


@register("sum")
def do_sum(value, attribute=None, start=0):
    getter = _attr_getter(attribute)
    return sum((getter(item) for item in value), start)


# ------------------------------------------------------------- sequences

@register("length", "count")
def do_length(value):
    try:
        return len(value)
    except TypeError:
        return len(list(value))


@register("first")
def do_first(value):
    for item in value:
        return item
    return Undefined("first item")


@register("last")
def do_last(value):
    items = list(value)
    if not items:
        return Undefined("last item")
    return items[-1]


@register("list")
def do_list(value):
    if isinstance(value, str):
        return list(value)
    return list(value)


@register("reverse")
def do_reverse(value):
    if isinstance(value, str):
        return value[::-1]
    return list(reversed(list(value)))


@register("sort")
def do_sort(value, reverse=False, case_sensitive=False, attribute=None):
    """Stable sort; strings compare case-insensitively unless ``case_sensitive``."""
    key = _sort_key(case_sensitive, _attr_getter(attribute))
    return sorted(value, key=key, reverse=reverse)


@register("dictsort")
def do_dictsort(value, case_sensitive=False, by="key", reverse=False):
    """Sort a dict's items by key (or ``by="value"``) into a list of pairs."""
    if by not in ("key", "value"):
        raise FilterArgumentError("dictsort: 'by' must be 'key' or 'value'")
    pos = 0 if by == "key" else 1
    key = _sort_key(case_sensitive, lambda item: item[pos])
    return sorted(value.items(), key=key, reverse=reverse)


@register("unique")
def do_unique(value, case_sensitive=False, attribute=None):
    """Items in first-seen order without duplicates."""
    key = _sort_key(case_sensitive, _attr_getter(attribute))
    seen = set()
    out = []
    for item in value:
        k = key(item)
        if k not in seen:
            seen.add(k)
            out.append(item)
    return out


@register("join")
def do_join(value, d="", attribute=None):
    getter = _attr_getter(attribute)
    return to_text(d).join(to_text(getter(item)) for item in value)


@register("batch")
def do_batch(value, linecount, fill_with=None):
    """Split into lists of ``linecount`` items; pad the last with ``fill_with``."""
    linecount = int(linecount)
    if linecount < 1:
        raise FilterArgumentError("batch: linecount must be positive")
    batches = []
    current = []
    for item in value:
        current.append(item)
        if len(current) == linecount:
            batches.append(current)
            current = []
    if current:
        if fill_with is not None:
            current.extend([fill_with] * (linecount - len(current)))
        batches.append(current)
    return batches


@register("map")
def do_map(value, attribute):
    getter = _attr_getter(attribute)
    return [getter(item) for item in value]


@register("default", "d")
def do_default(value, default_value="", boolean=False):
    """``default_value`` when ``value`` is undefined (or falsy, with ``boolean``)."""
    if isinstance(value, Undefined) or (boolean and not value):
        return default_value
    return value


# ------------------------------------------------------------------ tests

@register_test("defined")
def test_defined(value):
    return not isinstance(value, Undefined)


@register_test("undefined")
def test_undefined(value):
    return isinstance(value, Undefined)


@register_test("none")
def test_none(value):
    return value is None


@register_test("even")
def test_even(value):
    return value % 2 == 0


@register_test("odd")
def test_odd(value):
    return value % 2 == 1


@register_test("divisibleby")
def test_divisibleby(value, num):
    return value % num == 0


@register_test("string")
def test_string(value):
    return isinstance(value, str)


@register_test("number")
def test_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


@register_test("iterable")
def test_iterable(value):
    try:
        iter(value)
    except TypeError:
        return False
    return True


@register_test("sameas")
def test_sameas(value, other):
    return value is other


@register_test("lower")
def test_lower(value):
    return to_text(value).islower()


@register_test("upper")
def test_upper(value):
    return to_text(value).isupper()
