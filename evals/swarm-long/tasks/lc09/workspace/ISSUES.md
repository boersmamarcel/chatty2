# Open issues

Five open issues, reported by the release engineering team. They are
independent of each other. Each lists its acceptance criteria; the behaviour
described there is what will be checked, including every edge case listed.

---

## Issue 1: pre-release versions are ordered as plain text

Reported by: release manager

`sort` puts `1.0.0-rc.10` before `1.0.0-rc.9`, and `1.0.0-beta.11`
before `1.0.0-beta.2`. The ordering rules in the `verspec.version`
docstring (Semantic Versioning section 11) are not what `Version` does.
Also `Version.parse` accepts `1.0.0-rc..1` and `1.0.0-rc.01`, which are
not valid versions.

Acceptance (`verspec.version.Version`):

- Ordering follows the module docstring. This chain is strictly
  increasing: `1.0.0-1 < 1.0.0-2 < 1.0.0-10 < 1.0.0-alpha < 1.0.0-alpha.1 <
  1.0.0-alpha.beta < 1.0.0-beta < 1.0.0-beta.2 < 1.0.0-beta.11 <
  1.0.0-rc.1 < 1.0.0 < 1.0.1-rc.0 < 1.0.1`.
- Build metadata is still ignored: `1.0.0-rc.1+b.7 == 1.0.0-rc.1`, and equal
  versions have equal hashes.
- `Version.parse` raises `VersionError` for an empty pre-release or build
  identifier (`1.0.0-rc..1`, `1.0.0-rc.`, `1.0.0+b..1`, `1.0.0+`) and for a
  numeric pre-release identifier with a leading zero (`1.0.0-rc.01`,
  `1.0.0-01`). `1.0.0-0`, `1.0.0-rc.0`, `1.0.0-0a` and build identifiers
  with leading zeros (`1.0.0+001`) stay valid.

## Issue 2: caret constraints on 0.x versions allow breaking upgrades

Reported by: platform team

`^0.2.3` matches `0.9.0`. Before 1.0.0 every minor release may break the
API, and the `verspec.constraints` docstring says so: with major 0 the
first non-zero part counts as the "major".

Acceptance (`verspec.constraints.parse_constraint`):

- `^1.2.3` -> `>=1.2.3, <2.0.0` (unchanged).
- `^0.2.3` -> `>=0.2.3, <0.3.0`; `^0.0.3` -> `>=0.0.3, <0.0.4`;
  `^0.0.0` -> `>=0.0.0, <0.0.1`.
- The lower bound keeps a pre-release: `^0.2.3-rc.1` matches
  `0.2.3-rc.1` and `0.2.9` but not `0.3.0`.
- Caret items combine with other items as before (`^0.2.3, !=0.2.5`).

## Issue 3: bumping a version keeps parts that should be reset

Reported by: release manager

`bump 1.2.3 minor` prints `1.3.3`, and `bump 1.3.0-rc.2 minor` prints
`1.4.0-rc.2`. The rules are written down in the `verspec.bump` docstring.

Acceptance (`verspec.bump.bump(version, part)`):

- Releases: `1.2.3` -> major `2.0.0`, minor `1.3.0`, patch `1.2.4`.
- Pre-releases are bumped to their release when they are a pre-release of
  that part: `1.2.3-rc.1` patch -> `1.2.3`; `1.3.0-rc.2` minor ->
  `1.3.0`; `2.0.0-rc.1` major -> `2.0.0`. Otherwise the part is
  incremented as for a release: `1.2.3-rc.1` minor -> `1.3.0`,
  `1.2.3-rc.1` major -> `2.0.0`, `1.3.0-rc.2` major -> `2.0.0`.
- The result never has build metadata (`1.2.3+b.5` patch -> `1.2.4`), and
  never a pre-release unless `part == "prerelease"` (unchanged behaviour).
- An unknown part still raises `ValueError`.

## Issue 4: the resolver picks pre-releases and chokes on registry junk

Reported by: platform team

`max ^1.2.0 1.2.5 1.3.0-beta.1` picks the beta, although pre-releases must
only be installed when asked for. The registry also lists tags such as
`latest` among the versions, which crash `max_satisfying` with
`VersionError`, and passing the constraint as a string fails with
`AttributeError`.

Acceptance (`verspec.resolve.max_satisfying(versions, constraint,
include_prerelease=False)`):

- Pre-release versions are never returned unless
  `include_prerelease=True`; then they compete like any other version.
- Entries of `versions` that are not valid versions are skipped.
- `constraint` may be a `Constraint` or a constraint string (parsed with
  `parse_constraint`; a malformed string raises `ConstraintError`).
- Returns the highest matching `Version` (when two entries differ only in
  build metadata, the one listed first), or None.

## Issue 5: lock files with comments, odd spacing or duplicates

Reported by: CI maintainers

The lock file format in the `verspec.lockfile` docstring allows trailing
comments and is case-insensitive in package names, but `parse_lock` fails
on `left-pad==1.3.0  # pinned` with a bare `VersionError`, keeps `Flask`
and `flask` as two different packages, silently lets a second pin of the
same name overwrite the first, and a line without `==` crashes with an
unhelpful `ValueError` that names no line.

Acceptance (`verspec.lockfile.parse_lock`):

- Trailing comments are ignored; spaces around `==` are allowed; names are
  stored stripped and lower-cased.
- A package pinned twice (names compared case-insensitively) raises
  `LockError` with the message `"line <n>: duplicate package '<name>'"`
  (`<name>` lower-cased, `<n>` the line of the second pin).
- A line that is not `name==version` (no `==`, more than one `==`, an empty
  name or version) or whose version is invalid raises `LockError` with a
  message starting `"line <n>: "`.
- Line numbers start at 1 and count every line of the text, including blank
  and comment lines.
