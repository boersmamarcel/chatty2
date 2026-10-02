# Skill: architecture-review

Description: Bring one architecture document, an ADR or a design doc, to acceptance by repeated blank review. One proposer drafts and revises it from the code. Each round, a fresh maintainability reviewer, a fresh security reviewer and a fresh devil's advocate read the current text cold and check it against the code. The loop stops when a round has 0 must-fix findings. A verifier then checks the final polish, and the human decides the questions only they can. Use it for a request such as "write an ADR for …" or "write a design doc for …". Do not use it for code changes. Experimental.

## Inputs (from the task)

- **The goal.** What the document must decide or describe, word for word from the human.
- **The mode.** An **ADR** when the goal is one decision between alternatives; a **design doc** when it describes how a component works. Pick it from the goal.
- **The path.** `docs/adr/ADR-NNNN-slug.md` for an ADR (the proposer picks the next free number), `docs/design/<component>.md` for a design doc; or the path the task names. The *review file* is the same path with `.md` replaced by `.review.md`.

## Merging

The proposer works on its own branch, and nobody else sees its document until you merge that branch: a reviewer started before the merge reviews a file that does not exist. So whenever the proposer's answer ends with an `evidence` block, your very next tool call is `git_merge` with `{"branch": "<the branch line of that block>", "no_ff": true}`, before any other delegation. The one exception is the polish (step 6). Without an `evidence` block (the workspace is not a git repository) there is nothing to merge. On a conflict, list the files and stop, not converged; never resolve it yourself. Never read the document yourself to check it is there. If you have no `git_merge` tool in a git repository, stop before the first delegation and reply that the run needs the git tools (`--enable git`).

## Steps

1. **Draft.** Delegate to `arch-proposer` with the goal, the mode and the path: "Write the <mode> at <path>." It returns `proposer.json`. Then call `git_merge` on its branch (see *Merging*). If `human_questions` is not empty, ask the human each one with `ask_user` and give each answer to the proposer word for word in its next prompt.
2. **Review round N** (N = 1 after the draft). Delegate to `arch-maint-reviewer`, then `arch-sec-reviewer`, then `arch-devils-advocate`, one at a time. Each prompt names:
   - the document's path, which is read-only for reviewers, and its mode;
   - the goal;
   - this round's persona for that reviewer, from the lists below. Never reuse a persona.

   Never give a reviewer an earlier review, the proposer's answers, or another reviewer's output. Each reviewer returns `review.json`. The round is not over until all three have answered: never go to step 3 before `arch-devils-advocate` has returned.
3. **Count.** `M(N)` is the sum of the three reviewers' `must_fix`. If `M(N)` is 0 and all three reviewers returned a review, the loop **converged**: go to step 6. Skip step 4 and start no new round: the should-fix findings go to the polish.
4. **Revise.** Delegate to `arch-proposer` with all three reviews' `findings`, verbatim and labelled by reviewer, and the round number: "Verify each finding against the code, then accept, accept in part, or reject it with evidence, and revise <path>." Then call `git_merge` on its branch. Subtract its `rejected_must_fix` from `M(N)`: a must-fix rejected with evidence does not count. If that leaves `M(N)` at 0, the loop **converged**: go to step 6. Add one line of steering to the prompt only when a pattern repeats across rounds:
   - the same kind of must-fix twice in a row: "raise the level; state the invariant and move the mechanism to its issue";
   - a claim stronger than its mechanism: "claim less and name an owner";
   - a list that keeps growing: "replace the enumeration with one rule plus a named owner";
   - the body growing past what the goal needs: "move mechanics out of the document".

   Handle `human_questions` as in step 1.
5. **Loop rule.** After step 4 of round N, check these in order:
   - `N` is 3 or more and neither round `N-1` nor round `N` lowered the count (`M(N-1) >= M(N-2)` and `M(N) >= M(N-1)`): stop, **not converged** (no progress). Go to step 6.
   - A must-fix in round `N` is one the proposer accepted in an earlier round: stop, **not converged** (a fix that did not hold). Go to step 6.
   - `N` is 10: stop, **not converged** (the 10-round cap). Go to step 6.
   - Otherwise go to step 2 with N + 1.
6. **Polish.** Delegate to `arch-proposer`: "Apply the last round's should-fix findings and cheap nits to <path> with minimal edits, add no new guarantee, do one consistency read-through, and list `sections_edited`." Do not merge its branch yet.
7. **Verify.** Delegate to `arch-verifier` with the path and the range `<branch>~1..<branch>` (`<branch>` from the polish's `evidence` block): "Check every hunk of this diff of <path>." On `PASS`, merge the branch. On `FAIL`, delegate the verifier's `blockers` verbatim to `arch-proposer` ("Fix these in <path>; change nothing else."), and verify its branch the same way: merge it on `PASS`; on a second `FAIL`, ask the human with `ask_user` whether to keep the polish, and merge nothing unless they say so. Without an `evidence` block the polish cannot be verified; say so in the review file.
8. **Review file.** Delegate to `arch-proposer` with the review file's path: "Write <review path> with exactly these sections, and write no other file: `## Review record`, `## Human decisions`, `## Open questions for the human`, `## Polish check`." Give it the record: one line per round (`Round <N>: <M(N)> must-fix`), then `Rounds: <N>.` and either `Converged: round <N> found no must-fix.` or `Not converged: <no progress | a fix that did not hold | the 10-round cap>.`; each human decision taken; the open questions (every question the human did not answer and, when not converged, the last round's must-fix findings verbatim); and the polish verdict (`PASS`, `FAIL: <blockers>`, or `not verified: <why>`). Then call `git_merge` on its branch.
9. **Deliver.** Reply with the following, and nothing else:
   - the document's path and the review file's path;
   - converged or not converged, the rounds run, and the must-fix count per round;
   - each human decision taken;
   - the live issues the reviews found in current code, which are facts, not document text.

   Acceptance is the human's step: an ADR's status stays `proposed`. Never call a not-converged review done.

## Personas (one per round, in order)

**Maintainability:** senior architect; long-time Rust maintainer; new implementer planning the first step; test and CI owner; principal engineer (acceptance); pragmatic engineering lead; protocol designer; distributed-systems SRE.

**Security:** application-security engineer; adversarial red-teamer; acceptance sign-off walking every trust boundary (STRIDE); capability and confused-deputy specialist; multi-tenant cloud engineer; billing-fraud analyst; penetration tester for AI agents (prompt injection, approval phishing).

**Devil's advocate:** proponent of the simplest alternative; proponent of doing nothing; vendor of an off-the-shelf product; maintainer of the code this replaces; engineer who must ship it in a week; skeptic of the premise.

When a list runs out, start again from the top with "and you have not seen this document before".

## Questions for the human

Ask with `ask_user` as soon as a question comes up, never saved up for the end, and give the proposer's recommendation with it. If `ask_user` is not available or brings no answer, the question goes to the review file's `## Open questions for the human`, and the loop goes on without it.

## When a step fails

- **A reviewer's delegation fails** (an error, a timeout, an invalid handoff): delegate to the same reviewer once more with a new persona. If it fails again, record the gap ("<reviewer> returned no valid review in round <N>.") for the review file's open questions; the round does not converge, even at 0 must-fix. Go to step 4 with the reviews you have.
- **The proposer fails:** delegate once more with the same prompt. If it fails again, stop, not converged, and report to the human where the document stands.
- **The verifier fails twice:** the polish is not verified; merge nothing, and say so in the review file.
- **A rejected finding comes back three rounds in a row:** ask the human whether it is a real requirement or a preference, and pass the answer on.

## Rules

- You edit nothing, and you never decide a product or governance question. Those go to the human through `ask_user`, with the proposer's recommendation.
- Reviews reach the proposer verbatim. You never combine, soften or drop a finding, and you never count a should-fix as a must-fix or the other way round.
- Blank means blank: a reviewer sees only the document, the code and the goal, never the history.
- Convergence is 0 must-fix in the same round, after the rejections with evidence. Nothing else counts, however close.
- The document stays a standalone record: no rounds, no review history. The history lives only in the review file.
