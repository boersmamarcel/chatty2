"""Plain-text throttling report.

:func:`render` turns a :class:`~throttle.metrics.Metrics` into a fixed-width
table, one row per tenant plus a ``TOTAL`` row::

    tenant          allowed   denied   deny%
    acme                 12        4    25.0
    initech               3        1    25.0
    TOTAL                15        5    25.0

Columns: tenant name left-aligned in 12 characters, then allowed, denied and
deny% right-aligned in 9, 9 and 8 characters. deny% always shows one decimal.
The rows are ordered so that the most throttled tenants come first.
"""

from .metrics import deny_percent

HEADER = ("tenant", "allowed", "denied", "deny%")
_ROW = "%-12s%9s%9s%8s"


def _pct(value):
    return "%.1f" % value


def format_row(stats):
    """One table row for a :class:`~throttle.metrics.TenantStats`."""
    return _ROW % (stats.tenant, stats.allowed, stats.denied,
                   _pct(deny_percent(stats.allowed, stats.denied)))


def order_rows(rows):
    """Order tenant rows for the report: most denied requests first."""
    return sorted(rows, key=lambda s: (s.denied, s.tenant), reverse=True)


def render(metrics, top=None):
    """Render the report for ``metrics``.

    :param top: when given, only the first ``top`` tenant rows are shown; the
        ``TOTAL`` row always covers every tenant.
    :returns: the table as a string, lines joined with ``"\\n"``, ending with
        a newline.
    """
    rows = order_rows([metrics.stats(t) for t in metrics.tenants()])
    if top is not None:
        if top < 0:
            raise ValueError("top must not be negative")
        rows = rows[:top]
    lines = [_ROW % HEADER]
    lines.extend(format_row(stats) for stats in rows)
    lines.append(format_row(metrics.totals()))
    return "\n".join(lines) + "\n"


def summary(metrics):
    """One-line summary, e.g. ``"3 tenants, 20 decisions, 5 denied (25.0%)"``."""
    totals = metrics.totals()
    count = len(metrics.tenants())
    return "%d tenant%s, %d decisions, %d denied (%s%%)" % (
        count, "" if count == 1 else "s", totals.total, totals.denied,
        _pct(deny_percent(totals.allowed, totals.denied)))
