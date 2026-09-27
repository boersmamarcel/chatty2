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
    from temperature import convert
    close = lambda a, b: abs(a - b) < 1e-6
    assert close(convert(100, "C", "F"), 212)
    assert close(convert(32, "F", "C"), 0)
    assert close(convert(5, "C", "C"), 5)
    assert close(convert(0, "C", "K"), 273.15)
    assert close(convert(0, "K", "F"), -459.67)
    assert close(convert(300, "K", "C"), 26.85)
    assert close(convert(212, "F", "K"), 373.15)
    assert raises(ValueError, convert, 1, "X", "C")
    assert raises(ValueError, convert, -300, "C", "F")
    assert raises(ValueError, convert, -1, "K", "C")
    assert raises(ValueError, convert, -500, "F", "K")


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
