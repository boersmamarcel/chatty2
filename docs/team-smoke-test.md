# Team smoke test

**When to read this:** You are about to touch the broker, `--team`, the
worker tree, or the `coder-reviewer` preset and want one end-to-end run that
says whether a headless leader can still delegate, review, merge and pass a
verifier. This is the run that found five product bugs in one evening
(2026-09-13, AGE-400 to AGE-404) before it became a script (ADR-0011 C14,
AGE-408).

Scope is deliberately minimal: one task, one team, one reward. Task sets,
arms, statistics and cloud runs belong to harbor-chatty (AGE-34).

## What it runs

```bash
bash scripts/team-smoke.sh
```

One command, no arguments. It:

1. Builds `target/release/chatty-tui` when it is missing or older than the
   sources (or uses `CHATTY_TUI=<path>`).
2. Builds the `chatty-team-smoke` image (`scripts/team-smoke/Dockerfile`:
   `ubuntu:24.04` plus `python3`, `git`, `ca-certificates`).
3. Writes a throwaway `HOME` under `target/team-smoke/run-<stamp>/home/`:
   `providers.json` pointing at Ollama, `models.json` with the leader model
   (`extra_params.think=false`) and the coder model, and the
   `coder-reviewer` preset copied into `~/.local/share/chatty/teams/` with
   the two models and the verification command
   (`python3 -m unittest discover -s tests -t . -v`) filled in. The
   compiled-in preset carries neither, and `--team` looks in the data
   directory before the presets, so the run exercises AGE-407's search order
   and AGE-406's evidence hook without editing the task text.
4. Creates the fixture repo from `scripts/team-smoke/fixture/`: a bank
   account module whose `withdraw` allows overdrafts and negative amounts,
   one passing `test_deposit`, and `tests/__init__.py` so `unittest`
   discovery works.
5. Runs the leader as your uid in the container, under a 15-minute timeout:
   `chatty-tui --headless --team coder-reviewer --auto-approve --workspace /work -m "<task>"`.
   The task names the fix and the tests to add and says "Never edit files
   yourself"; `--team` injects `read_skill coder-reviewer and follow it` and
   the verification command on the first turn.
6. Runs `scripts/team-smoke/verify.sh` in a second container on the tree the
   leader left: the tests on the merged working tree, hidden checks
   (`deposit` still returns the balance; `withdraw` raises `ValueError` on
   0, negative and excess; a successful withdrawal returns the new balance),
   at least four tests defined, no unmerged `sub-agent/*` branches. It prints
   `REWARD=0|1` and the script exits accordingly.
7. Prints a one-screen summary (also saved as `summary.txt` in the run
   directory): worker processes spawned per role, `invoke_agent` delegations
   per agent, the reviewer's first line(s), the merge commit, wall time, and
   the verifier's output.

Everything the run produced stays in `target/team-smoke/run-<stamp>/`:
`leader.out` (the leader's answer), `leader.err` (the trace), `ps.log`,
`verify.log`, `home/` and `work/`.

Knobs, all optional: `CHATTY_TUI`, `OLLAMA_URL` (default
`http://172.17.0.1:11434`, the Docker bridge address of the host),
`LEADER_MODEL` (default `qwen3:14b`, also the reviewer's), `CODER_MODEL`
(default `qwen3:4b`), `TEAM_SMOKE_TIMEOUT` (seconds, default 900).

## Prerequisites

- Docker, usable by your user.
- Ollama on the host, reachable from a container: Ollama's default bind is
  `127.0.0.1:11434`, which the Docker bridge cannot reach, so run it with
  `OLLAMA_HOST=0.0.0.0:11434` (or point `OLLAMA_URL` at wherever it
  listens). `curl http://172.17.0.1:11434/api/version` from the host is the
  quick check.
- The two models pulled: `ollama pull qwen3:14b` and `ollama pull qwen3:4b`.
- `OLLAMA_NUM_PARALLEL` ≥ 2, so a worker's request is not queued behind the
  leader's open turn.
- A GPU with room for both models at once (about 12 GB for the defaults).

## Reading the result

A passing run (2026-09-13, 188 s) looks like: one `local-coder` process and
one `local-reviewer` process, two or three delegations (coder, reviewer,
and the reviewer again to verify the merged tree), `APPROVE` as the
reviewer's first line, a `Merge branch 'sub-agent/local-coder-0'` commit,
and `REWARD=1`.

Small local models flake. Two passing runs out of three consecutive is
acceptable for this smoke test; a run that fails on a 500 from Ollama or on a
timeout is a host problem, not a product one, so check the traps below before
reading a red run as a regression.

Measured 2026-09-14 on this script (chatty2 `f3cbc434` plus the script):
three consecutive runs gave rewards 0, 1, 1 in 9 s, 191 s and 181 s. The
failure modes seen while the script was being built, all in the leader or
the coder rather than in the broker:

- the leader restates the criteria and ends its turn without delegating
  (the 9 s run);
- the leader hands the reviewer a prompt without the coder's branch name,
  so the reviewer diffs its own empty `sub-agent/local-reviewer-N` branch
  and answers `BLOCKED`, repeatedly, until the 50-turn budget ends the run;
- the leader drops the "add tests" criterion when it restates a prose task,
  so the coder fixes `withdraw` without tests and the reviewer approves
  (the task text is a numbered list for that reason);
- the coder's report ends up committed as `answer.txt` on its branch, and
  a `venv/` it created was committed with it by the commit-on-exit hook;
  the verifier does not score stray files, but a merged tree can carry them.

## Traps already known

- **Ollama's OpenAI-compatible URL.** Pointing `--openai-compat-url` at
  Ollama's `/v1` needed AGE-403's normalization so discovery and chat agree;
  the script sidesteps the question by writing an `ollama` provider into
  `providers.json`, which the worker processes read from the same mounted
  `HOME`.
- **A GPU full from another model.** If something else already holds the
  GPU, the worker's model fails to load and the delegation ends with
  `500 Internal Server Error: model failed to load`; the leader retries,
  blocks, and the run times out. Check `ollama ps` and `nvidia-smi` before a
  run and unload what you do not need.
- **A binary not named `chatty-tui`.** The broker finds the worker
  executable as `chatty-tui` next to the running binary
  (`chatty_core::tools::worker_executable`). The script mounts whatever
  `CHATTY_TUI` names at `/opt/chatty/chatty-tui`, so a renamed build works
  here, but a hand-run leader with another name spawns no workers.
- **Think mode on the coder.** `extra_params.think=false` is set on the
  leader model only. `qwen3:4b` with thinking off narrates instead of
  working; leave the coder model's `extra_params` empty.
- **The container keeps running after the timeout.** `timeout` kills the
  Docker client, not the container; the script force-removes the container
  by name on exit, so do not run two copies with the same name in parallel
  (the name carries the script's PID).

## Not in CI

The run needs a GPU and a local Ollama. The self-hosted runner has neither
in its runner environment, so this is a local gate: run it before opening a
PR that touches `crates/chatty-protocol-gateway`, `chatty-tui/src/participant/`,
`chatty-core/src/services/{team,virtual_agents,worker_tree}.rs` or the
preset under `crates/chatty-core/teams/`, and paste the summary in the PR.
