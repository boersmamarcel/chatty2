
## 2026-11-08 — AGE-547 fix round 1
- Ran cargo fmt --all; only profile.rs changed (assert! split).
- open_allows_public_hosts and redirect_to_private_ip_gated_by_toggle also fail on origin/main (b582ab55), no DNS in sandbox; left untouched.
- Checks: profile tests 20 pass / 2 pre-existing fail; clippy -D warnings clean; fmt --check clean.
