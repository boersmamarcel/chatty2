# The swarm-vs-single benchmark

Measures whether a Chatty team beats one harness agent with the same tools
and model, and at what cost. There are 30 tasks in three families (data
audit, code fix and review, research then write). Each task runs as arm
`single` (one agent, no tool profile) and as arm `swarm` (the family's frozen
team preset).

- **Protocol:** fixed before any real run in
  [`docs/research/swarm-vs-single-prereg.md`](../../docs/research/swarm-vs-single-prereg.md).
- **Spec:** vault `dev/projects/fabric-evals-and-gate.md` §4.1.
- **Issues:** built by EV-3 (AGE-670), run by EV-4 (AGE-671).

## Run it

```bash
scripts/swarm-bench/run.sh --provider openai-compat --model RedHatAI/Qwen3.8-27B-INT4 \
  --think false --run-id ev4-qwen38-27b
python3 scripts/swarm-bench/report.py target/swarm-bench/ev4-qwen38-27b \
  --out docs/research/swarm-vs-single-$(date +%F).md
```

`run.sh` builds `target/release/chatty-tui` when it is missing or older than
the sources. To use another binary, set `CHATTY_TUI=<path>` or pass
`--chatty-tui <path>`.

Results land in `target/swarm-bench/<run-id>/`: `meta.json`, and per task
and arm `runs/<task>/<arm>/` holding `result.json`, `answer.txt`,
`stderr.log`, `usage.json` and, for code tasks, `diff.patch`.

Re-running the same run id skips the runs it already finished, so an
interrupted batch resumes.

| Flag | Meaning |
| -- | -- |
| `--provider openai-compat\|fake` | `openai-compat` is a local vLLM, by default `http://172.17.0.1:8000/v1`. `fake` is the dry run's (below). There is no priced provider. |
| `--model <id>` | The server's model id |
| `--arm single\|swarm\|both` | Default `both`, run per task in counterbalanced order |
| `--think true\|false` | The model's thinking switch, for every agent of both arms |
| `--only a,b,c`, `--limit N` | A subset: named tasks, or the first N of the interleaved order (d01, c01, r01, d02, …) |
| `--base-url URL` | The server, e.g. another vLLM |
| `--metrics-url URL`, `--throttle-max N` | The shared-server throttle. It waits while vLLM's running + waiting requests are ≥ N (default 2). It reads `<base>/metrics` by default. |
| `--max-turns N` | The single arm's turn budget (default 40). Team agents keep their spec's budgets. |
| `--max-duration D`, `--run-timeout S` | Per-run budget (default `30m`) and the hard kill (default 2400 s) |
| `--run-id ID`, `--out DIR`, `--tasks DIR`, `--keep-work` | Results directory, results root, task set, and whether to keep the scratch workspaces |

**Throttle and meter.** Every model call goes through a local pass-through
meter. It counts tokens at the wire, every agent of a team included; that
count is the cost metric. On a vLLM, the meter also holds a request back, for
up to 90 s, while the server is busy.

## Task set

Each task lives in `tasks/<id>/` and holds:

- `task.json`: the family, title and prompt;
- `workspace/`: what the agent sees;
- `check.json`: the verifier's spec, which the agent never sees;
- `solution/`: a reference fix or brief, for the dry run. Code and research
  tasks only.

The data tasks' CSV files come from `gen_data_tasks.py` (seed 670).

`verify.py <task-dir> <workspace> <answer-file>` checks a run in one of three
ways:

- a data task's `VERDICT:` line must match the known label;
- a code task's tests must pass, with the test files unchanged;
- a research task's `brief.md` must hold every checklist fact and cite only
  corpus files.

The task set is frozen with the pre-registration, which records its SHA-256.

## Dry run

`cargo test -p chatty-tui --test swarm_bench` runs
`swarm_bench_dry_run` and `prereg_exists_and_is_frozen`.

`swarm_bench_dry_run` runs three tasks, one per family, on both arms against
the BI-0 fake model (`chatty_core::testing::fake_model`). The fake model
scripts every agent's turns. The leaders really spawn their workers, and the
verifiers and the report generator run for real.

`prereg_exists_and_is_frozen` pins the pre-registration's hash. Both tests
run in CI.
