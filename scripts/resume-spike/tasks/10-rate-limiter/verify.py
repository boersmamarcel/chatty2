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
    from limiter import RateLimiter
    now = [0.0]
    lim = RateLimiter(2, 10, lambda: now[0])
    assert lim.remaining() == 2
    assert lim.allow() and lim.allow()
    assert not lim.allow()
    assert lim.remaining() == 0
    for t in (1, 5, 9.5):
        now[0] = t
        assert not lim.allow(), t
    now[0] = 10
    assert lim.remaining() == 2
    assert lim.allow() and lim.allow() and not lim.allow()
    now[0] = 15
    assert lim.remaining() == 0
    now[0] = 20
    assert lim.remaining() == 2


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
