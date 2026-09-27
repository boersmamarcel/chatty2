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
    from slug import slugify
    assert slugify("Hello World") == "hello-world", slugify("Hello World")
    assert slugify("Hello -- World!") == "hello-world", slugify("Hello -- World!")
    assert slugify(" Hi ") == "hi", slugify(" Hi ")
    assert slugify("Rust 2024: Ed.") == "rust-2024-ed", slugify("Rust 2024: Ed.")
    assert slugify("a \t\n b") == "a-b"
    assert slugify("--x--") == "x"
    assert slugify("") == ""


if __name__ == "__main__":
    try:
        main()
    except Exception:  # any failure, including an import error
        traceback.print_exc(file=sys.stdout)
        print("FAIL")
        sys.exit(1)
    print("PASS")
