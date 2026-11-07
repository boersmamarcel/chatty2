# AGE-1052 plan

## Acceptance criteria
- templates/module/Cargo.toml renamed to templates/module/Cargo.toml.liquid
- workspace resolves (cargo metadata)
- CI template-generation job stays valid (cargo-generate strips .liquid)
- no other files change except text naming the old file

## Steps
1. [x] git mv templates/module/Cargo.toml templates/module/Cargo.toml.liquid (commit: this one)
2. [x] Checked cargo-generate.toml, README.md, ci.yml: nothing names the old file; no edits needed (commit: this one)
3. [x] Verified rename, cargo metadata, diff stat (commit: this one)
