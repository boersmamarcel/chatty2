#!/usr/bin/env python3
"""Verifier: judges the worktree in argv[1] after the follow-up. Exit 0 = pass."""
import os
import sys
import traceback

sys.path.insert(0, os.path.abspath(sys.argv[1]))
os.chdir(sys.argv[1])
sys.dont_write_bytecode = True


def raises(exc, fn, *args, **kwargs):
    try:
        fn(*args, **kwargs)
    except exc:
        return True
    except Exception as other:  # the wrong exception is a failure too
        print("expected %s, got %r" % (exc.__name__, other))
        return False
    return False


def main():
    from roman import to_roman, from_roman
    assert to_roman(1994) == "MCMXCIV"
    assert to_roman(3999) == "MMMCMXCIX"
    assert to_roman(4) == "IV"
    assert raises(ValueError, to_roman, 0)
    assert raises(ValueError, to_roman, 4000)
    assert from_roman("MCMXCIV") == 1994
    for n in range(1, 4000):
        assert from_roman(to_roman(n)) == n, n
    for bad in ("IIII", "VX", "IC", "", "ABC", "MMMM", "VV"):
        assert raises(ValueError, from_roman, bad), bad


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
