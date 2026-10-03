# Open issues

Seven open issues, collected from the teams that use `stencil` for e-mails,
reports and HTML pages. They are independent of each other. Each lists its
acceptance criteria; the behaviour described there is what will be checked,
including every edge case listed. Examples use `env = stencil.Environment()`
unless they say otherwise; `render(src, **vars)` means
`env.from_string(src).render(**vars)`.

---

## Issue 1: `loop.last` is never true, `loop.revindex` is off by one, and loops over generators crash

Reported by: newsletter team

`{% if not loop.last %}, {% endif %}` puts a comma after the last item too,
and a countdown with `loop.revindex` ends at 0 instead of 1. Passing a
generator (`items=(r for r in rows)`) as the loop iterable raises `TypeError`
as soon as the template uses `loop.length` or `loop.last`. We would also like
the two loop helpers we know from other engines.

Acceptance (`stencil.runtime.LoopContext`, used as `loop` in `{% for %}`):

- For a loop over N items, on the item at 0-based position i:
  `loop.index` = i + 1, `loop.index0` = i, `loop.revindex` = N - i (1 on
  the last item), `loop.revindex0` = N - i - 1 (0 on the last item),
  `loop.first` is true only for i = 0, `loop.last` is true only for
  i = N - 1, `loop.length` = N. A one-item loop is both first and last.
- All of the above work for any iterable, including generators and other
  one-shot iterators, not only lists.
- `loop.previtem` is the item of the previous iteration and `loop.nextitem`
  the item of the next one. On the first item `previtem`, and on the last
  item `nextitem`, is undefined: it prints as an empty string and
  `loop.previtem is defined` is false.
- `loop.cycle(*values)` returns `values[loop.index0 % len(values)]`, e.g.
  `{% for x in "abc" %}{{ loop.cycle("odd", "even") }} {% endfor %}` renders
  `odd even odd `.
- `loop.depth` / `loop.depth0` keep working as before (1 / 0 for the
  outermost loop, 2 / 1 for a loop nested in it), and an outer loop's
  variables are correct again after an inner loop has finished.

## Issue 2: whitespace control only half works

Reported by: e-mail templates

`{%- if x %}` removes the spaces before the tag but not the line break, so
our plain-text mails still have empty lines everywhere. The `-` marker is
also ignored on `{{ }}` and `{# #}` tags.

Acceptance (`stencil.lexer`):

- `{%- ... %}`, `{{- ... }}` and `{#- ... #}` remove all whitespace
  (spaces, tabs, `\n`, `\r`) directly before the tag, up to the previous
  non-whitespace character or tag.
- `{% ... -%}`, `{{ ... -}}` and `{# ... -#}` remove all whitespace directly
  after the tag, up to the next non-whitespace character or tag (also at
  the very end of the template).
- A marker only affects its own side: `"a \n{%- if true %} \nb{% endif %}"`
  renders `"a \nb"`.
- The marker is only a marker when it is the very first character after
  the opening delimiter (or the very last before the closing one). A minus
  separated by a space is part of the expression: `"[{{ -1 }}]"` renders
  `"[-1]"` and `"x {{ 5 - 3 }} y"` renders `"x 2 y"`.
- Example: `"<ul>\n  {%- for i in [1, 2] %}\n  <li>{{ i }}</li>\n  {%- endfor %}\n</ul>"`
  renders `"<ul>\n  <li>1</li>\n  <li>2</li>\n</ul>"`.

## Issue 3: autoescaping double-escapes and `|safe` does nothing

Reported by: web team

With `Environment(autoescape=True)`, `{{ "<" }}` prints `&amp;lt;` instead of
`&lt;`, apostrophes are not escaped at all (they break our
`value='{{ x }}'` attributes), and `{{ html|safe }}` is escaped anyway.

Acceptance:

- `stencil.escaping.escape(value)` returns a `Markup` in which `&`, `<`,
  `>`, `"` and `'` are replaced by `&amp;`, `&lt;`, `&gt;`, `&#34;` and
  `&#39;`, each character escaped exactly once (`escape("&lt;")` is
  `"&amp;lt;"`, `escape("<")` is `"&lt;"`). Non-strings are converted with
  `str()` first (`escape(42)` is `"42"`). Values that are already `Markup`
  (or have an `__html__` method) are returned unchanged.
- `Markup + str` and `str + Markup` escape the plain `str` side and return
  `Markup`: `Markup("<b>") + "<"` is `Markup("<b>&lt;")`. `Markup + Markup`
  escapes nothing.
- `Markup(sep).join(items)` escapes every item that is not `Markup` and
  returns `Markup`: `Markup("<br>").join(["<", Markup("<i>")])` is
  `"&lt;<br><i>"`.
- The `safe` filter returns a `Markup`, so with autoescaping on
  `{{ "<b>"|safe }}` renders `<b>`.
- The `escape` filter (alias `e`) returns a `Markup`: with autoescaping
  off `{{ "<a>"|e }}` renders `&lt;a&gt;`; with autoescaping on it also
  renders `&lt;a&gt;` (not escaped twice), and `{{ "<b>"|safe|e }}`
  renders `<b>`.

## Issue 4: syntax errors point at the wrong place

Reported by: editor integration

Our editor plugin jumps to the position in `TemplateSyntaxError`, but the
lexer reports `line 0` for errors on the first line, everything after a
Windows (`\r\n`) or old Mac (`\r`) line break is on the wrong line, and the
message format is not the `file:line:col: message` format the plugin (and
every compiler) uses.

Acceptance:

- `stencil.utils.position_of(source, offset)` returns 1-based
  `(lineno, col)`; the first character of a template is at `(1, 1)`.
  Line breaks are `\n`, `\r\n` (one line break, not two) and a lone `\r`.
  The character right after a line break is in column 1. A tab counts as
  one column.
  Examples: `position_of("ab\ncd", 4)` is `(2, 2)`,
  `position_of("a\r\n\r\nb", 5)` is `(3, 1)`, `position_of("a\rb", 2)` is
  `(2, 1)`.
- `TemplateSyntaxError` keeps `message` (the bare message), `lineno`, `col`
  and `name` attributes; `str(error)` is `"<name>:<lineno>:<col>: <message>"`,
  where the name is `<string>` for templates without a name (created with
  `from_string`). Example: `env.from_string("a\n  {% frob %}")` raises an
  error with `lineno == 2`, `col == 3`, `message == "unknown tag 'frob'"`
  and `str(error) == "<string>:2:3: unknown tag 'frob'"`.
- The positions follow the rules in the `stencil.parser` docstring (errors
  inside a tag point at its opening delimiter; an unclosed block at the tag
  that opened it): for a template `page.html` with source `"x\r\n{{ y"`,
  `str(error)` is `"page.html:2:1: unclosed output tag"`.

## Issue 5: `round` and `filesizeformat` round wrong

Reported by: invoicing

`{{ 2.5|round }}` prints `2.0` and `{{ 1000|filesizeformat }}` prints
`1000 Bytes`. The module docstring of `stencil.numbers` already describes the
rounding we want; `format_number` and `percent` do it right.

Acceptance (`stencil.numbers.round_value`, used by the `round` filter):

- Always returns a `float` (`{{ 7|round }}` renders `7.0`).
- `method="common"` (the default) rounds halves away from zero, on the
  decimal value the number prints as: `2.5` -> `3.0`, `0.5` -> `1.0`,
  `-2.5` -> `-3.0`, `2.675` with precision 2 -> `2.68`, `1.005` with
  precision 2 -> `1.01`. Numeric strings are accepted (`"2.5"` -> `3.0`).
- `method="ceil"` / `"floor"` round up / down, also exactly on the decimal
  value: `1.1` ceil at precision 2 stays `1.1`, `0.29` floor at precision 2
  stays `0.29`, `-2.71` ceil at precision 1 is `-2.7`, floor is `-2.8`.
- Any other method still raises `FilterArgumentError`.

Acceptance (`stencil.numbers.filesizeformat`, the `filesizeformat` filter):

- `1` -> `"1 Byte"`; other sizes below the base -> `"<n> Bytes"`
  (`0` -> `"0 Bytes"`, `999` -> `"999 Bytes"`).
- The base is 1000 (`kB`, `MB`, `GB`, ...), or 1024 with `binary=True`
  (`KiB`, `MiB`, ...). A size equal to a unit already uses that unit:
  `1000` -> `"1.0 kB"`, `1000000` -> `"1.0 MB"`, binary `1024` ->
  `"1.0 KiB"`, binary `1048576` -> `"1.0 MiB"`, binary `1023` ->
  `"1023 Bytes"`.
- One decimal, rounded half up: `1250` -> `"1.3 kB"`, `1150` -> `"1.2 kB"`,
  `2500000` -> `"2.5 MB"`, binary `1536` -> `"1.5 KiB"`.

## Issue 6: template names are not normalised; `../` escapes the template folder

Reported by: security review

`env.get_template("../config/secrets.txt")` happily serves a file outside the
`FileSystemLoader` folder. Windows users write `partials\nav.html`, which is
not found. And `./a.html` and `a.html` are loaded and cached twice.

Acceptance:

- `stencil.loaders.split_template_path(name)` turns `\` into `/`, splits on
  `/`, drops empty and `.` segments and returns the list of segments:
  `"partials\\nav.html"` -> `["partials", "nav.html"]`, `"./a//b.html"` ->
  `["a", "b.html"]`. A `..` segment anywhere raises `TemplateNotFound`
  (even `"a/../b.html"`), and so does a name without any segment left
  (`""`, `"./"`).
- `FileSystemLoader.get_source` and `DictLoader.get_source` accept every
  form above: `DictLoader` looks up the segments joined with `/`, so
  `DictLoader({"a.html": ...}).get_source(env, "./a.html")` finds it, and
  both raise `TemplateNotFound` for names with `..`.
- `Environment.get_template` normalises the name the same way before
  using the cache and the loader: `get_template("./a.html")` and
  `get_template("a.html")` return the same `Template` object, whose `name`
  is `"a.html"` (`"dir\\b.html"` -> name `"dir/b.html"`).
  `{% include "./partials/nav.html" %}` works.

## Issue 7: support `{% for ... if condition %}`

Reported by: report designers

We want to filter items in the loop header instead of wrapping the body in
`{% if %}`, so that the loop counters only count the items that are shown.
Right now `{% for x in xs if x.active %}` is a syntax error.

Acceptance:

- `{% for target(s) in iterable if condition %}` is accepted; the condition
  is any expression and can use the loop target(s) (also with unpacking,
  `{% for k, v in pairs if v %}`) and every other variable in scope. Items
  for which it is false are skipped.
- `loop.index`, `loop.index0`, `loop.first` and `loop.length` count only
  the items that pass: `{% for x in [1, 5, 2, 7] if x > 1 %}{{ loop.index }}/{{ loop.length }} {% endfor %}`
  renders `1/3 2/3 3/3 `.
- The `{% else %}` block renders when no item passes the condition (also
  when the iterable is not empty), and only then.
- The condition cannot use `loop` (it is evaluated before the loop counters
  exist). A missing condition after `if` is a `TemplateSyntaxError`.
- Loops without `if` behave as before.
