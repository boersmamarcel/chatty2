import re


def _parse(v):
    main, dash, pre = v.partition("-")
    if not re.fullmatch(r"[0-9]+(\.[0-9]+)*", main) or (dash and not pre):
        raise ValueError("bad version: %r" % v)
    nums = [int(p) for p in main.split(".")]
    while nums and nums[-1] == 0:
        nums.pop()
    parts = None
    if pre:
        if not re.fullmatch(r"[A-Za-z0-9.]+", pre):
            raise ValueError("bad pre-release: %r" % v)
        parts = [int(p) if p.isdigit() else p for p in re.findall(r"[0-9]+|[A-Za-z]+", pre)]
    return nums, parts


def _cmp(x, y):
    return (x > y) - (x < y)


def compare_versions(a, b):
    (na, pa), (nb, pb) = _parse(a), _parse(b)
    if na != nb:
        return _cmp(na, nb)
    if pa is None or pb is None:
        return _cmp(pa is None, pb is None)
    for x, y in zip(pa, pb):
        if x != y:
            if type(x) != type(y):
                return -1 if isinstance(x, int) else 1
            return _cmp(x, y)
    return _cmp(len(pa), len(pb))
