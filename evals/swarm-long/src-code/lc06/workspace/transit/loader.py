"""Load a network from the plain-text network format.

The format is a small INI-like file with comma separated rows::

    # Harbour city tram network
    [stops]
    id, name, zone
    CEN, Central, 1
    MUS, Museum, 1
    HAR, Harbour, 2

    [links]
    from, to, line, minutes, oneway
    CEN, MUS, T1, 4
    MUS, HAR, T1, 6, yes

    [transfers]
    stop, minutes
    MUS, 3

Rules
-----
* Blank lines and lines whose first non-blank character is ``#`` are ignored.
* A line ``[name]`` starts a section; the known sections are ``stops``,
  ``links`` and ``transfers``.  The first row of every section is its header
  and is skipped (it is checked to start with the expected column name).
* Fields are separated by commas; surrounding whitespace is removed.  A field
  may be quoted with double quotes to contain a comma (``"Quay, North"``).
* ``zone`` is optional (an empty field means "no zone").
* ``oneway`` is optional: ``yes``/``true``/``1`` or ``no``/``false``/``0``/empty,
  case-insensitive.
* ``minutes`` must be a positive whole number.
* Sections may appear in any order: all stops are known before links and
  transfers are checked.

Problems are collected, not raised one by one: :func:`load_network` raises a
single :class:`~transit.errors.NetworkFormatError` whose ``errors`` list holds
one ``"line N: message"`` string per problem, in line order.
"""

import csv
import io

from .errors import NetworkFormatError
from .model import Network

SECTIONS = ("stops", "links", "transfers")

HEADERS = {
    "stops": ("id", "name", "zone"),
    "links": ("from", "to", "line", "minutes", "oneway"),
    "transfers": ("stop", "minutes"),
}

REQUIRED_FIELDS = {"stops": 2, "links": 4, "transfers": 2}

TRUE_FLAGS = ("yes", "true", "1")
FALSE_FLAGS = ("no", "false", "0", "")


def _norm_id(raw):
    """Canonical form of a stop id: surrounding whitespace removed, upper case."""
    return raw.strip().upper()


def _split(line):
    """Split one row into stripped fields (honours double quotes)."""
    reader = csv.reader([line], skipinitialspace=True)
    return [field.strip() for field in next(reader)]


def _parse_minutes(raw):
    try:
        value = int(raw)
    except ValueError:
        return None
    if value <= 0:
        return None
    return value


def _parse_flag(raw):
    flag = raw.strip().lower()
    if flag in TRUE_FLAGS:
        return True
    if flag in FALSE_FLAGS:
        return False
    return None


def read_rows(text):
    """Split the file into ``(section, lineno, fields)`` rows.

    Returns ``(rows, errors)``.  Header rows, comments and blank lines are
    dropped; structural problems (row outside a section, unknown section,
    wrong header) are reported in ``errors``.
    """
    rows = []
    errors = []
    section = None
    expect_header = False
    for lineno, raw in enumerate(io.StringIO(text), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("[") and line.endswith("]"):
            name = line[1:-1].strip().lower()
            if name not in SECTIONS:
                errors.append("line %d: unknown section [%s]" % (lineno, name))
                section = None
                expect_header = False
            else:
                section = name
                expect_header = True
            continue
        if section is None:
            errors.append("line %d: row outside of a known section" % lineno)
            continue
        fields = _split(line)
        if expect_header:
            expect_header = False
            if not fields or fields[0].lower() != HEADERS[section][0]:
                errors.append("line %d: expected the [%s] header row starting with %r"
                              % (lineno, section, HEADERS[section][0]))
            continue
        if len(fields) < REQUIRED_FIELDS[section]:
            errors.append("line %d: expected at least %d fields, got %d"
                          % (lineno, REQUIRED_FIELDS[section], len(fields)))
            continue
        if len(fields) > len(HEADERS[section]):
            errors.append("line %d: expected at most %d fields, got %d"
                          % (lineno, len(HEADERS[section]), len(fields)))
            continue
        rows.append((section, lineno, fields))
    return rows, errors


def load_network(text):
    """Parse the network format and return a :class:`~transit.model.Network`.

    Raises :class:`~transit.errors.NetworkFormatError` listing every problem.
    """
    rows, errors = read_rows(text)
    network = Network()

    # Pass 1: stops.
    seen = {}
    for section, lineno, fields in rows:
        if section != "stops":
            continue
        raw_id = fields[0]
        if not raw_id:
            errors.append("line %d: empty stop id" % lineno)
            continue
        if raw_id in seen:
            errors.append("line %d: duplicate stop %r" % (lineno, _norm_id(raw_id)))
            continue
        seen[raw_id] = lineno
        name = fields[1]
        if not name:
            errors.append("line %d: stop %r has no name" % (lineno, _norm_id(raw_id)))
            continue
        zone = fields[2] if len(fields) > 2 and fields[2] else None
        stop_id = _norm_id(raw_id)
        if network.has_stop(stop_id):
            network.stop(stop_id).name = name
            network.stop(stop_id).zone = zone
        else:
            network.add_stop(stop_id, name, zone)

    # Pass 2: links and transfers.
    for section, lineno, fields in rows:
        if section == "links":
            a, b, line = fields[0], fields[1], fields[2]
            problems = []
            for stop_id in (a, b):
                if not network.has_stop(stop_id):
                    problems.append("line %d: unknown stop %r" % (lineno, stop_id))
            if not line:
                problems.append("line %d: empty line name" % lineno)
            minutes = _parse_minutes(fields[3])
            if minutes is None:
                problems.append("line %d: minutes must be a positive integer, got %r"
                                % (lineno, fields[3]))
            oneway = _parse_flag(fields[4]) if len(fields) > 4 else False
            if oneway is None:
                problems.append("line %d: bad oneway flag %r" % (lineno, fields[4]))
            if not problems and a == b:
                problems.append("line %d: link from %r to itself" % (lineno, a))
            if problems:
                errors.extend(problems)
                continue
            network.add_link(a, b, line, minutes, oneway=oneway)
        elif section == "transfers":
            stop_id = fields[0]
            minutes = None
            try:
                minutes = int(fields[1])
            except ValueError:
                pass
            if not network.has_stop(stop_id):
                errors.append("line %d: unknown stop %r" % (lineno, stop_id))
            elif minutes is None or minutes < 0:
                errors.append("line %d: transfer minutes must be a whole number >= 0, got %r"
                              % (lineno, fields[1]))
            else:
                network.set_transfer_time(stop_id, minutes)

    if errors:
        errors.sort(key=_line_number)
        raise NetworkFormatError(errors)
    return network


def _line_number(message):
    """Sort key: the N of a ``line N: ...`` message (stable for equal N)."""
    head = message.split(":", 1)[0]
    try:
        return int(head.split()[1])
    except (IndexError, ValueError):
        return 0


def load_network_file(path, encoding="utf-8"):
    """Read ``path`` and parse it with :func:`load_network`."""
    with io.open(path, encoding=encoding) as handle:
        return load_network(handle.read())


def dump_network(network):
    """Serialise a network back to the text format (round-trips with load)."""
    out = ["[stops]", "id, name, zone"]
    for stop in network.stops():
        name = stop.name
        if "," in name or '"' in name:
            name = '"%s"' % name.replace('"', '""')
        out.append("%s, %s, %s" % (stop.id, name, stop.zone or ""))
    out.append("")
    out.append("[links]")
    out.append("from, to, line, minutes, oneway")
    for record in network.records:
        out.append("%s, %s, %s, %d, %s" % (record.a, record.b, record.line, record.minutes,
                                          "yes" if record.oneway else "no"))
    transfers = [(stop_id, network.transfer_time(stop_id, None)) for stop_id in network.stop_ids()]
    transfers = [(s, m) for s, m in transfers if m is not None]
    if transfers:
        out.append("")
        out.append("[transfers]")
        out.append("stop, minutes")
        for stop_id, minutes in transfers:
            out.append("%s, %d" % (stop_id, minutes))
    return "\n".join(out) + "\n"
