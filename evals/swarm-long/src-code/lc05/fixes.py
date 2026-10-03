FIXES = {
    1: [
        ("throttle/token_bucket.py",
         "            self._tokens = self._tokens + elapsed * self.refill_rate\n",
         "            self._tokens = min(float(self.capacity), self._tokens + elapsed * self.refill_rate)\n"),
        ("throttle/token_bucket.py",
         """        if cost <= 0:
            raise ValueError("cost must be positive")
""",
         """        if cost <= 0:
            raise ValueError("cost must be positive")
        if cost > self.capacity:
            raise ValueError("cost %s exceeds bucket capacity %s" % (cost, self.capacity))
"""),
    ],
    2: [
        ("throttle/retry.py",
         "    return int(round(seconds))\n",
         "    return max(1, int(math.ceil(seconds)))\n"),
        ("throttle/limiter.py",
         "        return format_headers(self.limit, self.remaining, self.reset_after, retry_after)\n",
         "        remaining = max(0, int(math.floor(self.remaining)))\n"
         "        return format_headers(self.limit, remaining, self.reset_after, retry_after)\n"),
    ],
    3: [
        ("throttle/sliding_window.py",
         "        while self._log and self._log[0] < horizon:\n",
         "        while self._log and self._log[0] <= horizon:\n"),
        ("throttle/sliding_window.py",
         """        allowed = len(self._log) + cost <= self.limit
        for _ in range(int(cost)):
            self._log.append(now)
        return allowed
""",
         """        if len(self._log) + cost > self.limit:
            return False
        for _ in range(int(cost)):
            self._log.append(now)
        return True
"""),
    ],
    4: [
        ("throttle/plans.py",
         """_SLASH_FORM = re.compile(r"^(\\d+)\\s*/\\s*(\\d*)\\s*([a-z]+)$")
""",
         """_SLASH_FORM = re.compile(r"^(\\d+)\\s*/\\s*(\\d*)\\s*([a-z]+)$", re.IGNORECASE)
_PER_FORM = re.compile(r"^(\\d+)\\s+per\\s+(\\d*)\\s*([a-z]+)$", re.IGNORECASE)
"""),
        ("throttle/plans.py",
         """    match = _SLASH_FORM.match(cleaned)
    if match is None:
        raise ConfigError("invalid rate '%s'" % cleaned)
    count, multiplier, unit = match.groups()
""",
         """    match = _SLASH_FORM.match(cleaned) or _PER_FORM.match(cleaned)
    if match is None:
        raise ConfigError("invalid rate '%s'" % cleaned)
    count, multiplier, unit = match.groups()
    unit = unit.lower()
"""),
        ("throttle/config.py",
         """        raise ConfigError(["line %d: %s" % (lineno, message)])
""",
         """        self.errors.append((lineno, message))
"""),
        ("throttle/config.py",
         """            raise ConfigError(["line %d: %s" % item for item in self.errors])
""",
         """            ordered = sorted(self.errors, key=lambda item: item[0])
            raise ConfigError(["line %d: %s" % item for item in ordered])
"""),
    ],
    5: [
        ("throttle/periods.py",
         """    first = datetime.datetime(year, month, 1, tzinfo=UTC)
    return first + datetime.timedelta(days=anchor_day - 1)
""",
         """    last_day = calendar.monthrange(year, month)[1]
    return datetime.datetime(year, month, min(anchor_day, last_day), tzinfo=UTC)
"""),
        ("throttle/periods.py",
         "        return self.start <= moment <= self.end\n",
         "        return self.start <= moment < self.end\n"),
    ],
    6: [
        ("throttle/keys.py",
         """from .errors import InvalidKeyError
""",
         """import re

from .errors import InvalidKeyError

_UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
"""),
        ("throttle/keys.py",
         """    return segment.isdigit()
""",
         """    return segment.isdigit() or _UUID.match(segment) is not None
"""),
        ("throttle/keys.py",
         """    path = str(route).strip().lower()
    if not path.startswith("/"):
        path = "/" + path
    segments = path.split("/")
    out = []
    for segment in segments:
        if _is_identifier(segment):
            out.append(ID_PLACEHOLDER)
        else:
            out.append(segment)
    return "/".join(out)
""",
         """    path = str(route).strip()
    for mark in "?#":
        path = path.split(mark, 1)[0]
    path = path.lower()
    out = []
    for segment in path.split("/"):
        if not segment:
            continue
        if _is_identifier(segment):
            out.append(ID_PLACEHOLDER)
        else:
            out.append(segment)
    return "/" + "/".join(out)
"""),
    ],
    7: [
        ("throttle/metrics.py",
         """import math
""",
         """import math
from decimal import ROUND_HALF_UP, Decimal
"""),
        ("throttle/metrics.py",
         """    total = allowed + denied
    if total == 0:
        return 0.0
    return round(denied * 100.0 / total, 1)
""",
         """    total = allowed + denied
    if total == 0:
        return Decimal("0.0")
    exact = Decimal(denied * 100) / Decimal(total)
    return exact.quantize(Decimal("0.1"), rounding=ROUND_HALF_UP)
"""),
        ("throttle/report.py",
         """    return sorted(rows, key=lambda s: (s.denied, s.tenant), reverse=True)
""",
         """    return sorted(rows, key=lambda s: (-s.denied, -deny_percent(s.allowed, s.denied), s.tenant))
"""),
    ],
}
