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
    from cart import Cart
    c = Cart()
    c.add("tea", 2.5, 2)
    c.add("tea", 9.99)
    c.add("cup", 4.0)
    assert c.total() == 11.5, c.total()
    assert raises(ValueError, c.add, "x", -1)
    assert raises(ValueError, c.add, "x", 1, 0)
    assert raises(KeyError, c.remove, "nope")
    c.apply_code("SAVE10")
    assert c.total() == 10.35, c.total()
    assert raises(ValueError, c.apply_code, "BOGUS")
    assert c.total() == 10.35
    c.apply_code("FLAT5")
    assert c.total() == 6.5
    c.remove("tea")
    assert c.total() == 0.0


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
