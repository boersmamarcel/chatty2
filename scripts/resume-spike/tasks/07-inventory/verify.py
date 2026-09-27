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
    from inventory import Inventory
    inv = Inventory()
    inv.add("apple", 5)
    inv.add("pear", 1)
    inv.add("fig", 2)
    assert inv.low_stock(3) == ["fig", "pear"]
    assert inv.total() == 8
    assert raises(ValueError, inv.remove, "apple", 6)
    assert inv.count("apple") == 5
    for bad in (0, -1, 1.5):
        assert raises(ValueError, inv.add, "apple", bad), bad
        assert raises(ValueError, inv.remove, "apple", bad), bad
    assert raises(ValueError, inv.remove, "kiwi", 1)
    inv.remove("pear", 1)
    assert "pear" not in inv.items and inv.count("pear") == 0
    assert inv.low_stock(3) == ["fig"]
    assert inv.total() == 7


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
