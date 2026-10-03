#!/usr/bin/env python3
"""Minimum detectable effects for the long multi-part benchmark (EV-7, AGE-826).

    mde_long.py --n 12 --sd 0.25

- Sub-part score (primary): the paired difference's MDE at two-sided
  alpha = 0.05 and power 0.80, from the paired t-test's power, which the
  exact sign-flip permutation test that report_long.py runs tracks closely;
  `--sd` is the SD of the per-task score difference.
- Full-task pass (key secondary): the exact McNemar test's MDE by exact
  enumeration, for discordance in the opposite direction q = 0, 0.05, 0.10
  (the same method as the EV-3 pre-registration).

Python 3.6+, stdlib only.
"""

import argparse


def comb(n, k):
    out = 1
    for i in range(1, k + 1):
        out = out * (n - k + i) // i
    return out


def mcnemar_p(b, c):
    n = b + c
    if n == 0:
        return 1.0
    k = min(b, c)
    return min(1.0, 2 * sum(comb(n, i) for i in range(k + 1)) / float(2 ** n))


def mcnemar_power(n, d, q, alpha=0.05):
    p10, p01 = d + q, q
    p0 = 1 - p10 - p01
    if p0 < 0:
        return 0.0
    total = 0.0
    for b in range(n + 1):
        for c in range(n - b + 1):
            if b > c and mcnemar_p(b, c) < alpha:
                total += comb(n, b) * comb(n - b, c) * p10 ** b * p01 ** c * p0 ** (n - b - c)
    return total


# Student t quantiles (0.975, 0.80) by degrees of freedom.
T975 = {5: 2.571, 6: 2.447, 7: 2.365, 8: 2.306, 9: 2.262, 10: 2.228, 11: 2.201, 12: 2.179,
        13: 2.160, 14: 2.145, 15: 2.131, 16: 2.120, 17: 2.110, 18: 2.101, 19: 2.093}
T80 = {5: 0.920, 6: 0.906, 7: 0.896, 8: 0.889, 9: 0.883, 10: 0.879, 11: 0.876, 12: 0.873,
       13: 0.870, 14: 0.868, 15: 0.866, 16: 0.865, 17: 0.863, 18: 0.862, 19: 0.861}


def score_mde(n, sd):
    """The paired t-test's MDE, which the sign-flip permutation test matches
    closely at these n (it is exact under the null, with near-t power)."""
    return (T975[n - 1] + T80[n - 1]) * sd / n ** 0.5


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--n", type=int, required=True)
    p.add_argument("--sd", type=float, required=True, help="SD of the per-task score difference")
    args = p.parse_args()
    d = score_mde(args.n, args.sd)
    print("sub-part score: n=%d sd=%.2f MDE=%.2f (%.0f pp)" % (args.n, args.sd, d, 100 * d))
    for q in (0.0, 0.05, 0.10):
        m = 0.0
        while m < 0.95 and mcnemar_power(args.n, m, q) < 0.8:
            m += 0.01
        print("full pass: n=%d q=%.2f MDE=%.0f pp" % (args.n, q, 100 * m))


if __name__ == "__main__":
    main()
