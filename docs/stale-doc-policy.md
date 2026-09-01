# Stale-doc policy

**When to read this:** You changed code and need to know whether docs
must change in the same PR, or you found a page that no longer matches
the repo.

Draft wording (AGE-118 / DOC-51). Edit this file if a rule is wrong.

## Same-PR rule

If a change alters a **fact a page claims**, update that page in the
same PR. Do not rely on a follow-up workflow to notice.

A page is stale when a contributor or agent following it would do the
wrong thing, look in the wrong place, or miss a required step.

### Update in the same PR

| If you change… | Update… |
|---|---|
| Architecture, crate boundaries, or a documented pattern | The matching `docs/*.md` page |
| A how-to's steps or file paths | `docs-site/src/dev/guides/` |
| User-visible behavior that a user guide describes | `docs-site/src/user/` and `README.md` when the landing page claims it |
| CI commands, make targets, or contributor conventions | `AGENTS.md` and/or `CLAUDE.md` |
| A crate's purpose or public surface | That crate's `README.md` |
| A new file under `docs/` | A row in [`INDEX.md`](INDEX.md) |
| A new hand-written site page | An entry in `docs-site/src/SUMMARY.md` |
| The tools catalog (hand-maintained list in `scripts/gen-docs-reference.sh`) | That script — generated markdown is not committed |

Generated reference pages (`tools-catalog`, CLI flags, env vars, events,
settings, …) live in gitignored `docs/generated/` and are rebuilt by
`make docs-gen` / docs CI. Same-PR work is only needed when the
**generator** would still emit the old fact — for example the hardcoded
tool list in `scripts/gen-docs-reference.sh`.

### Do not update (and do not file drift)

- Internal refactors that leave documented facts true
- Formatting, comments, or test-only changes
- Dependency bumps that do not change contributor or user guidance
- Facts the generator will pick up on the next docs build

When unsure, update the page. A short same-PR edit is cheaper than a
stale agent following the old steps.

## Automation vs this policy

Workflows are a **safety net**, not a license to skip the same-PR rule.
Prefer updating docs yourself when you already know they drifted.

| Check | Scope | Action | Opt out |
|---|---|---|---|
| [`update-agent-docs.yml`](../.github/workflows/update-agent-docs.yml) | `AGENTS.md`, `CLAUDE.md` | Follow-up PR after merge to `main` | `skip-agent-docs` or `documentation` |
| [`update-readme.yml`](../.github/workflows/update-readme.yml) | `README.md` (user-facing only) | Follow-up PR after merge | `skip-readme` or `documentation` |
| [`docs.yml`](../.github/workflows/docs.yml) + `make docs-check-nav` | `docs/INDEX.md`, `SUMMARY.md` completeness | CI fails the PR | — |
| `docs.yml` + `make docs-check-links` | Internal / GitHub path links | CI fails the PR | — |
| `docs.yml` + `make docs-gen` | Generated reference + `llms.txt` | Rebuilt on docs build; no drift PR | — |

`update-agent-docs` does **not** scan architecture pages, user guides,
how-tos, crate READMEs, or generated reference. AGE-116's nav-drift
check lives in `docs.yml` (`scripts/check-docs-nav-drift.sh`), not in
that workflow.

Skip labels (`skip-agent-docs`, `skip-readme`) mean "I handled this" or
"this merge has no guidance impact" — not "docs may stay wrong."

## Reporting leftover drift

File a GitHub issue with the
[Doc drift](https://github.com/boersmamarcel/chatty2/issues/new?template=doc-drift.yml)
template when:

1. You found a stale page and you are **not** already fixing it in a PR, or
2. Automation missed it (`update-agent-docs` / `update-readme` did not
   open a PR, or the page is outside their scope).

Search open `documentation` issues first. Do not file for something the
same-PR rule already covers on an open PR.

Planned rewrite work (new pages, ownership, `docs:*` labels) stays in
Linear project **Chatty developer documentation**. Promote a GitHub drift
report there only when it needs an `owner:*` assignment or epic tracking.

## Reviewers

Block merge when the diff changes a claimed fact and the matching page
is untouched. `make docs-check-nav` and the link checker only catch
missing nav rows and broken links — not wrong sentences.
