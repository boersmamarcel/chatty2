"""Text formatting of amounts, dates and plain-text tables for reports."""

from decimal import Decimal, ROUND_HALF_UP

from .money import minor_units, quantize, to_decimal


def format_amount(amount, currency=None):
    """Format ``amount`` with the minor units of ``currency``.

    Rounds half-up first; negative amounts keep a leading minus sign.
    """
    value = quantize(amount, currency)
    return "{0:,.{1}f}".format(value, minor_units(currency))


def format_date(day):
    """ISO date (``2024-03-31``); empty string for None."""
    return day.isoformat() if day is not None else ""


def pad(text, width, align="<"):
    """Pad ``text`` to ``width`` (``"<"`` left, ``">"`` right aligned)."""
    if align == ">":
        return text.rjust(width)
    return text.ljust(width)


def format_table(headers, rows, aligns=None):
    """Render a plain-text table.

    ``rows`` are lists of strings.  Column widths fit the widest cell (header
    included); cells are separated by two spaces; a rule of ``-`` the full
    table width follows the header.  Trailing spaces are stripped from every
    line.  Returns the lines joined with ``"\\n"`` (no trailing newline).
    """
    aligns = aligns or ["<"] * len(headers)
    widths = [len(h) for h in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(cell))
    lines = ["  ".join(pad(h, widths[i], aligns[i]) for i, h in enumerate(headers)).rstrip()]
    lines.append("-" * (sum(widths) + 2 * (len(widths) - 1)))
    for row in rows:
        lines.append("  ".join(pad(c, widths[i], aligns[i]) for i, c in enumerate(row)).rstrip())
    return "\n".join(lines)


def percent(part, whole, digits=1):
    """``part / whole`` as a percentage string rounded half-up, ``"-"`` when whole is zero."""
    whole = to_decimal(whole)
    if whole == 0:
        return "-"
    value = (to_decimal(part) * 100 / whole).quantize(Decimal(1).scaleb(-digits), rounding=ROUND_HALF_UP)
    return "%s%%" % value
