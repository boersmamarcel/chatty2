# stencil

A small text templating engine (Python 3, standard library only) for e-mails,
plain-text reports and HTML pages: `{{ expressions }}` with filters,
`{% if %}` / `{% for %}` / `{% set %}` / `{% with %}` / `{% include %}` /
`{% filter %}` statements, `{# comments #}`, whitespace control, optional HTML
autoescaping and file-system / in-memory template loaders.

    from stencil import Environment, FileSystemLoader
    env = Environment(loader=FileSystemLoader("templates"), autoescape=True)
    print(env.get_template("invoice.html").render(customer=..., lines=...))

The pipeline is lexer -> parser (AST in `stencil/nodes.py`) -> renderer; see
the module docstrings in `stencil/` for the details, and `ISSUES.md` for the
open issues.

## Tests

    python3 -m unittest discover -s tests -t .
