"""Plan configuration files.

The configuration format is a small INI dialect::

    # Lines starting with '#' or ';' are comments; blank lines are ignored.
    [plan free]
    rate = 10/s
    rate = 1000/hour
    algorithm = token_bucket
    burst = 20
    quota = 50000
    anchor_day = 1

    [plan pro]
    rate = 100/s
    algorithm = sliding_window

    [tenants]
    acme = pro
    initech = free

Inside a ``[plan NAME]`` section the keys are ``rate`` (repeatable, at least
one required), ``algorithm``, ``burst``, ``quota`` and ``anchor_day``. The
``[tenants]`` section maps tenant names to plan names; tenant names are
normalised with :func:`throttle.keys.normalise_tenant`.

:func:`parse_config` returns a :class:`Config`; problems are reported as a
:class:`~throttle.errors.ConfigError` whose messages are prefixed with
``"line N: "``.
"""

import re

from .errors import ConfigError, InvalidKeyError, UnknownPlanError
from .keys import normalise_tenant
from .plans import ALGORITHMS, Plan, parse_rate

_SECTION = re.compile(r"^\[\s*([^\]]*?)\s*\]$")
_INT_KEYS = ("burst", "quota", "anchor_day")


class Config(object):
    """Parsed configuration: ``plans`` (name -> Plan) and ``tenants``
    (normalised tenant -> plan name)."""

    def __init__(self, plans, tenants):
        self.plans = plans
        self.tenants = tenants

    def plan_for(self, tenant, default=None):
        """The :class:`Plan` of ``tenant``.

        Falls back to the plan named ``default`` when the tenant is not listed.
        Raises :class:`UnknownPlanError` when neither resolves.
        """
        name = self.tenants.get(normalise_tenant(tenant), default)
        if name is None or name not in self.plans:
            raise UnknownPlanError("no plan for tenant '%s'" % tenant)
        return self.plans[name]

    def __repr__(self):
        return "Config(plans=%r, tenants=%r)" % (sorted(self.plans), self.tenants)


class _PlanDraft(object):
    """Settings collected for one ``[plan NAME]`` section."""

    def __init__(self, name, lineno):
        self.name = name
        self.lineno = lineno
        self.rates = []
        self.settings = {}


class _Parser(object):
    def __init__(self, text):
        self.lines = text.splitlines()
        self.errors = []
        self.drafts = []
        self.plan_names = set()
        self.tenant_lines = []  # (lineno, tenant, plan name)
        self.section = None  # None, "tenants" or a _PlanDraft

    def error(self, lineno, message):
        """Report a problem found on line ``lineno``."""
        raise ConfigError(["line %d: %s" % (lineno, message)])

    def parse(self):
        for index, raw in enumerate(self.lines):
            lineno = index + 1
            line = raw.strip()
            if not line or line[0] in "#;":
                continue
            header = _SECTION.match(line)
            if header:
                self.open_section(lineno, header.group(1))
                continue
            if "=" not in line:
                self.error(lineno, "expected 'key = value'")
                continue
            key, value = line.split("=", 1)
            key = key.strip().lower()
            value = value.strip()
            if self.section is None:
                self.error(lineno, "setting outside of a section")
            elif self.section == "tenants":
                self.tenant_entry(lineno, key, value)
            else:
                self.plan_entry(lineno, self.section, key, value)
        plans = self.build_plans()
        tenants = self.build_tenants(plans)
        if self.errors:
            raise ConfigError(["line %d: %s" % item for item in self.errors])
        return Config(plans, tenants)

    def open_section(self, lineno, title):
        words = title.split()
        if len(words) == 1 and words[0].lower() == "tenants":
            self.section = "tenants"
            return
        if len(words) == 2 and words[0].lower() == "plan":
            name = words[1]
            if name in self.plan_names:
                self.error(lineno, "duplicate plan '%s'" % name)
                self.section = _PlanDraft(name, lineno)  # parsed, then dropped
                return
            self.plan_names.add(name)
            self.section = _PlanDraft(name, lineno)
            self.drafts.append(self.section)
            return
        self.error(lineno, "unknown section '%s'" % title)
        self.section = None

    def plan_entry(self, lineno, draft, key, value):
        if key == "rate":
            try:
                draft.rates.append(parse_rate(value))
            except ConfigError as exc:
                for message in exc.errors:
                    self.error(lineno, message)
        elif key == "algorithm":
            if value not in ALGORITHMS:
                self.error(lineno, "unknown algorithm '%s'" % value)
            else:
                draft.settings["algorithm"] = value
        elif key in _INT_KEYS:
            try:
                draft.settings[key] = int(value)
            except ValueError:
                self.error(lineno, "%s must be an integer" % key)
        else:
            self.error(lineno, "unknown key '%s'" % key)

    def tenant_entry(self, lineno, key, value):
        try:
            tenant = normalise_tenant(key)
        except InvalidKeyError as exc:
            self.error(lineno, str(exc))
            return
        self.tenant_lines.append((lineno, tenant, value))

    def build_plans(self):
        plans = {}
        for draft in self.drafts:
            if not draft.rates:
                self.error(draft.lineno, "plan '%s' has no rate" % draft.name)
                continue
            try:
                plans[draft.name] = Plan(draft.name, draft.rates, **draft.settings)
            except ConfigError as exc:
                for message in exc.errors:
                    self.error(draft.lineno, message)
        return plans

    def build_tenants(self, plans):
        tenants = {}
        for lineno, tenant, plan_name in self.tenant_lines:
            if plan_name not in self.plan_names:
                self.error(lineno, "tenant '%s' refers to unknown plan '%s'" % (tenant, plan_name))
                continue
            tenants[tenant] = plan_name
        return tenants


def parse_config(text):
    """Parse configuration ``text`` into a :class:`Config`."""
    return _Parser(text).parse()


def load_config(path):
    """Read and parse the configuration file at ``path`` (UTF-8)."""
    with open(path, encoding="utf-8") as handle:
        return parse_config(handle.read())
