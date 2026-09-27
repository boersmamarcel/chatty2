import itertools
import re


def encode(s):
    return "".join("%d%s" % (len(list(run)), ch) for ch, run in itertools.groupby(s))


def decode(s):
    if not re.fullmatch(r"(?:[1-9][0-9]*[A-Za-z])*", s):
        raise ValueError("malformed: %r" % s)
    return "".join(ch * int(n) for n, ch in re.findall(r"([0-9]+)([A-Za-z])", s))
