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
    from dates import days_between
    assert days_between("2024-03-01", "2024-03-04") == 3
    assert days_between("2024-02-28", "2024-03-01") == 2
    assert days_between("2024-03-04", "2024-03-01") == -3
    assert days_between("2024-03-01", "2024-03-04", business_only=True) == 1
    assert days_between("2024-03-04", "2024-03-11", business_only=True) == 5
    assert days_between("2024-03-11", "2024-03-04", business_only=True) == -5
    assert days_between("2024-03-02", "2024-03-04", business_only=True) == 0
    assert days_between("2024-03-05", "2024-03-05", business_only=True) == 0
    assert raises(ValueError, days_between, "2024-13-01", "2024-03-01")
    assert raises(ValueError, days_between, "yesterday", "2024-03-01")


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
