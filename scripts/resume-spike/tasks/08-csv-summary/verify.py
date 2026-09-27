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
    import tempfile
    from summary import summarize
    path = os.path.join(tempfile.mkdtemp(), "t.csv")
    with open(path, "w") as f:
        f.write("a,b,c,d\n1,x,2,\n3,y,,\n")
    got = summarize(path)
    assert set(got) == {"a", "c"}, got
    assert got["a"] == {"sum": 4.0, "mean": 2.0, "count": 2}, got["a"]
    assert got["c"] == {"sum": 2.0, "mean": 2.0, "count": 1}, got["c"]
    sample = summarize("data/sample.csv")
    assert set(sample) == {"price", "qty"}, sample
    assert abs(sample["price"]["sum"] - 1.25) < 1e-9 and sample["qty"]["count"] == 2


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
