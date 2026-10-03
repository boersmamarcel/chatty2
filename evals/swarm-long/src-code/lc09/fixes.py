"""Reference fixes for lc09 (verspec): exact replacements per issue."""

FIXES = {
    1: [
        ("verspec/version.py",
         """        major, minor, patch, pre, build = match.groups()
        return cls(int(major), int(minor), int(patch), _identifiers(pre), _identifiers(build))""",
         """        major, minor, patch, pre, build = match.groups()
        pre_ids = _identifiers(pre)
        build_ids = _identifiers(build)
        if any(not ident for ident in pre_ids + build_ids):
            raise VersionError("empty identifier in %r" % (text,))
        for ident in pre_ids:
            if ident.isdigit() and len(ident) > 1 and ident.startswith("0"):
                raise VersionError("leading zero in pre-release identifier of %r" % (text,))
        return cls(int(major), int(minor), int(patch), pre_ids, build_ids)"""),
        ("verspec/version.py",
         """            pre = (0, ".".join(self.prerelease))""",
         """            pre = (0, tuple((0, int(ident), "") if ident.isdigit() else (1, 0, ident)
                            for ident in self.prerelease))"""),
    ],
    2: [
        ("verspec/constraints.py",
         """def _caret(version):
    upper = Version(version.major + 1, 0, 0)""",
         """def _caret(version):
    if version.major > 0:
        upper = Version(version.major + 1, 0, 0)
    elif version.minor > 0:
        upper = Version(0, version.minor + 1, 0)
    else:
        upper = Version(0, 0, version.patch + 1)"""),
    ],
    3: [
        ("verspec/bump.py",
         """    if part == "major":
        return Version(version.major + 1, 0, 0)
    if part == "minor":
        return Version(version.major, version.minor + 1, version.patch, version.prerelease)
    if part == "patch":
        return Version(version.major, version.minor, version.patch + 1)""",
         """    pre = version.is_prerelease
    if part == "major":
        if pre and version.minor == 0 and version.patch == 0:
            return Version(version.major, 0, 0)
        return Version(version.major + 1, 0, 0)
    if part == "minor":
        if pre and version.patch == 0:
            return Version(version.major, version.minor, 0)
        return Version(version.major, version.minor + 1, 0)
    if part == "patch":
        if pre:
            return Version(version.major, version.minor, version.patch)
        return Version(version.major, version.minor, version.patch + 1)"""),
    ],
    4: [
        ("verspec/resolve.py",
         """    candidates = [_as_version(v) for v in versions]
    matching = [v for v in candidates if constraint.matches(v)]
    if not matching:""",
         """    if isinstance(constraint, str):
        constraint = parse_constraint(constraint)
    matching = []
    for value in versions:
        try:
            version = _as_version(value)
        except VersionError:
            continue
        if version.is_prerelease and not include_prerelease:
            continue
        if constraint.matches(version):
            matching.append(version)
    if not matching:"""),
    ],
    5: [
        ("verspec/lockfile.py",
         """    for line_no, raw in enumerate(text.splitlines()):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        name, version = line.split("==")
        pins[name.strip()] = Version.parse(version)""",
         """    for line_no, raw in enumerate(text.splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        parts = line.split("==")
        if len(parts) != 2 or not parts[0].strip() or not parts[1].strip():
            raise LockError("line %d: expected 'name==version', got %r" % (line_no, line))
        name = parts[0].strip().lower()
        try:
            version = Version.parse(parts[1].strip())
        except VersionError as exc:
            raise LockError("line %d: %s" % (line_no, exc))
        if name in pins:
            raise LockError("line %d: duplicate package '%s'" % (line_no, name))
        pins[name] = version"""),
    ],
}
