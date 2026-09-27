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
    from bank import Account
    a = Account(10)
    assert a.deposit(5) == 15
    assert raises(ValueError, a.deposit, 0)
    assert raises(ValueError, a.deposit, -1)
    b = Account(10)
    for bad in (0, -1, 11):
        assert raises(ValueError, b.withdraw, bad), bad
    assert b.balance == 10
    assert b.withdraw(4) == 6
    x, y = Account(10), Account(1)
    assert x.transfer(y, 4) == 6
    assert (x.balance, y.balance) == (6, 5)
    for bad in (0, -2, 7):
        assert raises(ValueError, x.transfer, y, bad), bad
        assert (x.balance, y.balance) == (6, 5)


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
