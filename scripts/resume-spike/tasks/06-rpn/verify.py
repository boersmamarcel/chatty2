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
    from rpn import evaluate
    assert evaluate("3 4 + 2 *") == 14
    assert evaluate("10 4 /") == 2.5
    assert evaluate("5 1 2 + 4 * + 3 -") == 14
    assert evaluate("-3 2 *") == -6
    for bad in ("1 +", "1 2", "1 x +", "", "+"):
        assert raises(ValueError, evaluate, bad), bad
    assert raises(ZeroDivisionError, evaluate, "1 0 /")


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
