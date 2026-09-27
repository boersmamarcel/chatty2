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
    from versions import compare_versions as cmp
    assert cmp("1.2.10", "1.2.9") == 1
    assert cmp("1.2", "1.2.0") == 0
    assert cmp("0.9", "1.0") == -1
    assert cmp("1.0.0-rc1", "1.0.0") == -1
    assert cmp("1.0.0", "1.0.0-rc1") == 1
    assert cmp("1.0.0-rc2", "1.0.0-rc10") == -1
    assert cmp("2.0.0-alpha", "2.0.0-beta") == -1
    assert cmp("1.0.1-alpha", "1.0.0") == 1
    assert cmp("1.0-rc1", "1.0.0-rc1") == 0
    for bad in ("x.y", "", "1..2", "1.2-"):
        assert raises(ValueError, cmp, bad, "1.0"), bad


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
