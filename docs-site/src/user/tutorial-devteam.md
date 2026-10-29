# Tutorial: an autonomous dev team that builds software story by story

**When to read this:** You have run [a small agentic team](./tutorial-team.md) and want to see how far a team can go unattended: five agents that turn a product brief into tested, reviewed, committed code, one user story at a time, for hours. This is the experimental end of teams. Plan an evening of mostly waiting, and read [Results, honestly](#results-honestly) before you start.

## What you will end up with

A project directory with a five-agent team (`devteam`), a product brief, and a small shell loop that runs the team in iterations. After every iteration the loop runs the test suite itself and feeds the result into the next prompt, so a red suite gets repaired before anything new is built. When you stop it you have a backlog of user stories, Gherkin feature files, plans, code, unit tests, BDD tests and a git history with one commit per piece of work, on a green `main`.

The example product is `logslice`, a command-line tool that filters and summarises JSON-lines log files. Substitute your own brief.

## Prerequisites

- `chatty-tui` ([install](./terminal.md#install)), version 0.7.8 or later. The leader's `--enable` flag reaching the workers, which this page relies on, arrived in the release after 0.7.8; on 0.7.8 itself, turn the shell and git on in the execution settings instead (see [Run it in a box](#2-run-it-in-a-box)).
- A model server: [Ollama](https://ollama.com), or any OpenAI-compatible server (vLLM, llama.cpp, LM Studio). Use the strongest model you can run; this team makes dozens of tool calls per story. The results below came from a 27B model on one GPU.
- Python 3.9 or later (with `venv`) and git in the environment that runs the team. The example product is written in Python, so the tests are pytest.
- A container, or a throwaway VM or user account. The run uses `--auto-approve`: read [step 2](#2-run-it-in-a-box) before you start it.

## How the team divides the work

| Agent | Tool profile | Job |
|-------|--------------|-----|
| `dev-lead` | `coordinator` | The leader. Delegates, merges each worker's branch, writes no code and runs no commands |
| `product-owner` | `coder`, shell off | Writes user stories, Gherkin feature files and the ordered `BACKLOG.md`. Never writes code |
| `developer` | `coder` | Writes a short plan, then the code |
| `tester` | `coder` | Writes pytest unit tests and pytest-bdd steps for the feature file and runs the whole suite |
| `code-reviewer` | `reviewer` | Reads the work against the acceptance criteria. First line of its answer: `APPROVE` or `REQUEST_CHANGES` |

One run of the leader is one story: story, plan and code, tests, review, at most one fix round, merge, tick the story in `BACKLOG.md`. Each worker gets its own git branch and the leader merges after every worker, so the next worker builds on the previous one's work. The mechanics are the ones from [the previous tutorial](./tutorial-team.md).

## Steps

### 1. Create the project

```bash
mkdir logslice && cd logslice
git init -q -b main
mkdir -p .chatty/agents .chatty/teams/devteam docs features src/logslice tests/unit tests/bdd
touch tests/__init__.py
printf '__pycache__/\n*.pyc\n.pytest_cache/\n*.egg-info/\n.venv\n.home\n' > .gitignore
```

You will create the Python virtual environment yourself in step 2, once, because the agents are told not to use the network. Write the product brief to `README.md`; the team reads it first. A brief states what the tool does and the quality bar, and nothing about stories, since breaking it down is the product owner's job:

```markdown
# logslice

A command-line tool that answers quick questions about JSON-lines log files:
one JSON object per line. Pure Python, standard library only. It must work on
files larger than memory (stream, never load the whole file).

* `logslice FILE...` (or `-` for stdin) prints the lines that match the filters.
* Filters: `--level warn`, a time window (`--since`, `--until`: ISO-8601 or
  relative like `15m`), `--where user.id=42`, and `--grep` on the message.
* `--count-by FIELD --top N` prints the most frequent values of a field.
* `--stats` prints counts per level, first and last timestamp, busiest minute.
* Lines that are not valid JSON are skipped and counted, never fatal.
* Gzip files are read transparently. Exit codes: 0 matched, 1 nothing, 2 usage error.

Quality bar: small functions, no network, no third-party runtime dependencies,
unit tests and pytest-bdd scenarios for every story.
```

Now the three project files that every agent reads. `AGENTS.md` is the team's shared rulebook: Chatty loads it into every agent's context, so the layout and the exact test command live here once:

```markdown
# How this team works on logslice

Read `README.md` first: it is the product brief.

## Layout

* `BACKLOG.md`: ordered stories, each `- [ ] S00N: As a …, I want …, so that …`; tick `[x]` when done.
* `docs/stories/S00N-<slug>.md`: one story with its acceptance criteria.
* `features/S00N-<slug>.feature`: the same criteria as Gherkin. Strict Gherkin: every step starts
  with a capitalised Given/When/Then/And/But (pytest-bdd rejects lowercase `and`, and one bad
  file makes the whole suite fail to collect).
* `docs/plans/S00N.md`: the developer's short plan before coding.
* `src/logslice/`: the code (standard library only). Entry point `logslice.cli:main`.
* `tests/unit/test_*.py` and `tests/bdd/test_*.py` (pytest-bdd steps bound to `features/*.feature`;
  `pytest.ini` sets `bdd_features_base_dir = features`, so use `scenarios("<file>.feature")`).

## Running the tests

Always run the whole suite from the repo root:

    PYTHONPATH=src .venv/bin/python -m pytest -q

Do not install anything else and do not use the network.

## Rules

* One story per cycle. Small functions named for what they do.
* Never read or write anything outside the repo.
* Commit your own work with `git_commit`, naming the story id (`S002: report empty values`).
* A story is done only when the whole suite passes and the reviewer approved it.
* Main must be green and checked out at the end of every run. A red suite is repaired first.
```

```bash
cat > pytest.ini <<'EOF'
[pytest]
bdd_features_base_dir = features
pythonpath = src
EOF
cat > pyproject.toml <<'EOF'
[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"

[project]
name = "logslice"
version = "0.1.0"
requires-python = ">=3.9"

[project.scripts]
logslice = "logslice.cli:main"
EOF
```

(Put the `README.md` and `AGENTS.md` text above into those files with your editor.)

### 2. Run it in a box

The loop below starts Chatty with `--auto-approve`, which lets every agent run shell commands and git operations without asking you. That is what makes an unattended run possible, and it means whatever the model decides to run, runs. **Do not do this on your everyday machine.** Use a container, a throwaway VM, or a separate user account that owns nothing you care about.

A container is the quickest. This image is all the team needs, apart from the `chatty-tui` binary, which you mount in:

```bash
cat > Dockerfile.devteam <<'EOF'
FROM ubuntu:24.04
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      python3 python3-venv git ca-certificates bash && rm -rf /var/lib/apt/lists/*
EOF
docker build -q -t devteam-box -f Dockerfile.devteam .
docker run --rm -it --network host --user "$(id -u):$(id -g)" -e HOME=/work/.home \
  -v "$PWD":/work -v "$(command -v chatty-tui)":/usr/local/bin/chatty-tui:ro \
  -w /work devteam-box bash
```

Run `mkdir -p .home` before `docker run`; `.home` is already in `.gitignore`. `--network host` lets the box reach a model server on your machine. Everything from here on runs inside the box's shell. Create the virtual environment there, so it matches the box's Python:

```bash
python3 -m venv .venv && .venv/bin/pip install -q pytest pytest-bdd
git config user.name devteam && git config user.email devteam@localhost
```

The `pip install` needs network access once; after that the agents never do. The git identity is there so commits work in a fresh box.

Two things to know about the shell tool in a box:

- **The shell sandbox.** Chatty runs shell commands inside a `bubblewrap` sandbox when it can. On Ubuntu 24.04 the default AppArmor setting blocks the user namespaces that sandbox needs, so commands run unsandboxed. Since 0.7.8, a headless run prints one line to stderr saying so and why. In a container that is expected, and the container is your boundary. If you would rather a run refuse to start unsandboxed, set `"sandbox_required": true` in `execution_settings.json`; Chatty then stops at startup with the reason, before any model call. See [Security & approvals](./security.md).
- **No one to ask.** In an unattended run (`--headless`), an approval request nobody can answer is denied at once instead of waiting for five minutes (0.7.8 and later). `--auto-approve` is what stops requests from arising in the first place.

Workers read `execution_settings.json` themselves, so a worker's shell and git come from those settings unless the leader's flags are forwarded: in the release after 0.7.8 the leader's `--enable`, `--disable` and `--only` reach every worker. A headless team also prints one stderr line per agent whose profile wants the shell or git that the run has off, which is the line to look for if a worker says it has no shell.

### 3. Write the team

Five agent files and a `team.json`. The preamble is where a team's behaviour lives, and several sentences in them that look fussy (the capitalised Gherkin, the red-suite rule, "merge after each worker") each answer a failure from real runs.

```bash
cat > .chatty/agents/dev-lead.toml <<'EOF'
[agent]
name = "dev-lead"
description = "Leads one story through story, plan, build, tests and review, and merges the work"
preamble = """You lead a small software team building logslice (read README.md and AGENTS.md first). You write no code yourself: you delegate, merge and report. Run exactly ONE story per run, in this order:

1. product-owner: write or refine the user story and its acceptance criteria (story file + Gherkin feature file) and keep BACKLOG.md up to date.
2. developer: write the plan docs/plans/S00N.md, then implement it in src/logslice/.
3. tester: write unit tests and pytest-bdd steps for the story's feature file, run the WHOLE suite with the command in AGENTS.md, and report the pass/fail counts.
4. code-reviewer: review against the acceptance criteria; the first line of its answer is APPROVE or REQUEST_CHANGES.
5. If REQUEST_CHANGES: send the reviewer's list to developer (and tester if tests are affected), then ask code-reviewer once more. At most one fix round.

Every worker works on its own git branch. After EACH worker returns, merge its branch into main with git_merge before you delegate to the next worker. If a merge conflicts, ask that worker to rebase its work on main and try again.

RED SUITE RULE: if the prompt says the suite is failing, or you find main red, your FIRST actions are to delegate the repair: tester for test or feature-file problems, developer for code problems, with the exact failing output from the prompt in the task. Never start a new story while main is red.

When the story is done, ask product-owner to tick it in BACKLOG.md. Finish with a short report: story id and title, files changed, the suite result, the reviewer's verdict, and the next story id."""

[tools]
profile = "coordinator"

[swarm]
delegates_to = ["product-owner", "developer", "tester", "code-reviewer"]

[budget]
max_agent_turns = 80
max_duration = "75m"
EOF
cat > .chatty/agents/product-owner.toml <<'EOF'
[agent]
name = "product-owner"
description = "Writes user stories with clear acceptance criteria and Gherkin scenarios, keeps the backlog"
preamble = """You are the product owner of logslice (README.md is the brief, AGENTS.md the layout). You write user stories in the form "As a <user>, I want <capability>, so that <benefit>", each with 3-6 concrete, testable acceptance criteria, and the same criteria as Gherkin scenarios (Given/When/Then/And/But, capitalised: pytest-bdd rejects lowercase `and`; concrete example files and expected output) in features/S00N-<slug>.feature. Keep stories small: one capability each. Keep BACKLOG.md as an ordered list of small stories, the most valuable first. Write only docs, BACKLOG.md and .feature files; never code. Before you hand a feature file on, re-read every step line: each must start with a capitalised Given/When/Then/And/But. Commit your files with git_commit, naming the story id. Reply with the story id, its title and the files written."""

[tools]
profile = "coder"
disable = ["shell", "code-exec", "docker-exec", "fetch"]

[swarm]
exposed = true
callers = ["dev-lead"]

[budget]
max_agent_turns = 25
max_duration = "20m"
EOF
cat > .chatty/agents/developer.toml <<'EOF'
[agent]
name = "developer"
description = "Plans and implements one story in small, readable Python"
preamble = """You are the developer of logslice (README.md is the brief, AGENTS.md the layout and the test command). For the story you are given: first write a short plan in docs/plans/S00N.md (files, functions, edge cases), then implement it in src/logslice/ with the standard library only. Keep functions small; follow the rules in AGENTS.md. Make sure `logslice.cli:main` works as the entry point. Run the whole suite with the command in AGENTS.md before you finish; fix what you broke. Commit with git_commit naming the story id. Reply with the files changed and the suite result."""

[tools]
profile = "coder"
disable = ["fetch", "docker-exec"]

[swarm]
exposed = true
callers = ["dev-lead"]

[budget]
max_agent_turns = 35
max_duration = "25m"
EOF
cat > .chatty/agents/tester.toml <<'EOF'
[agent]
name = "tester"
description = "Writes pytest unit tests and pytest-bdd steps for a story and runs the whole suite"
preamble = """You are the tester of logslice (AGENTS.md has the layout and the exact test command). For the story you are given: write unit tests in tests/unit/test_<topic>.py for the functions the developer added, and pytest-bdd step definitions in tests/bdd/test_<story>.py bound to the story's features/S00N-<slug>.feature with scenarios("<file>.feature") (pytest.ini sets bdd_features_base_dir = features, so give only the file name). Use tmp_path for example files. Feature files are strict Gherkin: steps start with Given/When/Then/And/But, capitalised; if one does not parse (pytest says so at collection), fix the .feature file yourself rather than working around it. Run the WHOLE suite with the command in AGENTS.md. If a test fails because the code is wrong, say exactly which behaviour is wrong; do not change src/. Commit your tests with git_commit naming the story id. Reply with the files written and the exact passed/failed counts from the last run."""

[tools]
profile = "coder"
disable = ["fetch", "docker-exec"]

[swarm]
exposed = true
callers = ["dev-lead"]

[budget]
max_agent_turns = 35
max_duration = "25m"
EOF
cat > .chatty/agents/code-reviewer.toml <<'EOF'
[agent]
name = "code-reviewer"
description = "Reviews a story's code and tests against its acceptance criteria; APPROVE or REQUEST_CHANGES"
preamble = """You review logslice work and edit nothing. Read the story file, its feature file, the plan, the code and the tests. Run the whole suite with the command in AGENTS.md. Check that every acceptance criterion is implemented and tested and that the code is simple. The first line of your answer is APPROVE or REQUEST_CHANGES, then a numbered list: what you checked, or exactly what must change (file, function, behaviour)."""

[tools]
profile = "reviewer"
disable = ["fetch", "docker-exec"]

[swarm]
exposed = true
callers = ["dev-lead"]

[budget]
max_agent_turns = 20
max_duration = "15m"
EOF
cat > .chatty/teams/devteam/team.json <<'EOF'
{
  "name": "devteam",
  "leader": "dev-lead",
  "agents": ["product-owner", "developer", "tester", "code-reviewer"],
  "max_agent_turns": 80
}
EOF
git add -A && git commit -q -m "Setup: brief, team and tooling for logslice"
```

Differences from the team in the previous tutorial: there is no `verification` command or playbook skill here. The suite command lives in `AGENTS.md` and the leader's preamble is the playbook, and the objective check happens in the loop, outside the team, in the next step. The `[budget]` in each spec caps that agent's tool rounds and wall-clock time, so one stuck worker cannot eat the iteration.

### 4. Run the loop

One iteration is one headless run of the leader:

```bash
chatty-tui --headless --team devteam --max-duration 75m --enable shell,git --auto-approve \
  --ollama http://localhost:11434 --model qwen3:14b \
  -m "<prompt>"
```

For an OpenAI-compatible server, replace the provider flags with `--openai-compat-url http://localhost:8000 --api-key KEY --model NAME`. `--enable shell,git` turns on the tool groups the coder profile needs. `--max-duration 75m` is the wall-clock limit for the iteration.

A single iteration stops after one story, so a small loop drives the rest. What matters is the gate: the loop runs the test suite itself, so the next prompt is based on what the code does and not on what the team says it did.

```bash
cat > run-loop.sh <<'EOF'
#!/usr/bin/env bash
# run-loop.sh: run the devteam in iterations until the backlog is done or MAX_ITER is reached.
set -u
PROVIDER=(--ollama http://localhost:11434 --model qwen3:14b)  # or --openai-compat-url URL --api-key KEY --model NAME
MAX_ITER=${MAX_ITER:-10}
SUITE='PYTHONPATH=src .venv/bin/python -m pytest -q'

prompt='Ask the product-owner for a BACKLOG.md of 6-8 small stories and the story and feature file for S001, then run the full cycle for S001.'
for i in $(seq 1 "$MAX_ITER"); do
  echo "=== iteration $i $(date +%T)"
  chatty-tui --headless --team devteam --max-duration 75m --enable shell,git \
    --auto-approve "${PROVIDER[@]}" -m "$prompt" | tee "run-$i.log" | tail -n 20

  # Objective gate: the suite, not the team's own report.
  git checkout -q main
  if out=$(bash -c "$SUITE" 2>&1); then
    suite_ok=1
    status="The test suite passes: $(echo "$out" | tail -n 1)."
  else
    suite_ok=0
    status="The test suite is FAILING. Repair it first, before any new work. Hand this output to the tester or developer:
$(echo "$out" | tail -n 40)"
  fi
  echo "$status" | head -n 1

  if [ "$suite_ok" = 1 ] && ! grep -q '^- \[ \]' BACKLOG.md 2>/dev/null; then
    echo "Backlog complete."; break
  fi
  prompt="$status
Run the full cycle for the first unticked story in BACKLOG.md."
done
EOF
chmod +x run-loop.sh
```

Start it with `./run-loop.sh` and leave it. `run-1.log`, `run-2.log` and so on hold each iteration's output; `git log --oneline` and `BACKLOG.md` show progress. To stop, press Ctrl+C, then check that `git status` is clean and `main` is checked out. Add `run-*.log`, `run-loop.sh` and `Dockerfile.devteam` to `.gitignore`, so the loop's own files do not dirty the tree.

The first prompt asks for the whole backlog and the first story's full cycle. Every later prompt is the same sentence, with the gate's result in front. If an iteration ends before any test exists, `pytest` exits non-zero and the gate reports the suite as failing; the next prompt then asks for the repair, which is harmless.

### 5. Read what it built

While it runs, the leader's hand-offs stream to stderr as in the previous tutorial: `invoke_agent`, `[agent: product-owner] ✓ completed`, `git_merge`, and so on. Between iterations, look at the work itself:

```bash
cat BACKLOG.md                       # [x] for done stories, [ ] for open ones
git log --oneline | head             # one commit per piece of work, naming the story id
cat features/S001-*.feature          # the acceptance criteria as scenarios
PYTHONPATH=src .venv/bin/python -m pytest -q
```

A feature file the product owner writes looks like this:

```gherkin
Feature: Filter log lines by level

  Scenario: Only lines at or above the minimum level are printed
    Given a log file "app.log" with an info line and an error line
    When I run "logslice --level warn app.log"
    Then the output contains only the error line
    And the exit code is 0
```

Read the stories early. A story that is too large is the commonest reason for a slow run; edit `BACKLOG.md` yourself, commit, and the next iteration picks it up.

## Lessons that make a long run work

Each of these cost real time in real runs.

- **Strict Gherkin.** Steps must start with a capitalised `Given`, `When`, `Then`, `And` or `But`. pytest-bdd rejects a lowercase `and`, and a single unparseable feature file makes the whole suite fail to collect, which turns every test red and cost hours. Say it in the product owner's preamble, in `AGENTS.md` and in the tester's, and let the tester fix a bad feature file.
- **`bdd_features_base_dir = features` in `pytest.ini`.** Without it the step files cannot find the feature files, and each agent invents its own path. With it, `scenarios("S001-x.feature")` works everywhere.
- **Repair a red suite first, with the output in hand.** A red `main` poisons every later story. The loop hands the failing output to the leader, and the leader's preamble says to delegate the repair to the tester or developer before anything else. Without the output, the team tends to spend its iteration investigating.
- **Keep stories small.** One capability each. Small stories finish inside a worker's turn budget, review cleanly and fail in ways that are easy to read.
- **Main green and checked out at the end of every iteration.** The loop runs `git checkout main` before the gate, and `AGENTS.md` makes it a rule. The next iteration then starts from known ground.
- **Check with a script, not with the team.** A team that reports "all tests pass" is sometimes right. The loop's gate does not care.

## Results, honestly

Two real runs of this team, both with a local model, `RedHatAI/Qwen3.8-27B-INT4` on one GPU served by vLLM:

| | Run 1 | Run 2 |
|--|-------|-------|
| Date | 26 Oct 2026 | 28-29 Oct 2026 |
| Product | `envcheck` | `logslice` |
| Setup | First attempt | With the fixes from run 1 |
| Duration | About 16.5 h of run time over 16 iterations (an 11.5 h session and a 5 h continuation), including about 5 h lost to setup problems that are now fixed | 8 h, 7 iterations |
| Stories | 6 of 7 ticked | 2 of 8 done |
| Tests passing at the end | 131 | 118 |
| Commits | 51 | 19 |

In run 2 the suite was red in iterations 1 and 2 and was repaired in iteration 3; `main` was green at the end. That is roughly 2.5 hours per story on this model.

What to take from that: throughput is limited by model speed. The team structure works, the loop recovers from a red suite, and the output is tested code in git, but a local model of this size is slow, and the stories it finishes per night are few. A faster or stronger model will do better; these numbers say nothing about one. Teams are experimental, so expect to read the logs, adjust the preambles and sometimes restart.

## Verify

- After step 4, iteration 1: `BACKLOG.md` lists six to eight stories, `docs/stories/S001-*.md` and `features/S001-*.feature` exist, and `git log` has commits naming `S001`.
- After a successful iteration: `PYTHONPATH=src .venv/bin/python -m pytest -q` passes on `main`, and S001 is ticked in `BACKLOG.md`.
- `chatty-tui --team devteam --headless -m "hello"` in the project directory finds the team. From another directory it fails naming the places it looked.

## Common issues

- **A worker says it has no shell, or commands are refused.** Check stderr for the per-agent warning that a profile wants the shell or git that the run has off. On releases that forward the leader's flags to workers, `--enable shell,git` is enough; otherwise enable the groups in the execution settings.
- **Every test fails at collection.** A `.feature` file does not parse. Run `PYTHONPATH=src .venv/bin/python -m pytest -q` by hand and read the first error; the usual cause is a lowercase `and`. Fix it, commit, and let the loop continue.
- **The iteration ends with nothing merged.** The leader hit its turn or time limit mid-story. The next prompt asks for the first unticked story again, so work continues; if it keeps happening, split the story.
- **The model server queues requests.** Workers on one local server run one after the other. That is expected and only costs wall-clock time.
- **A run refuses to start unsandboxed.** You set `sandbox_required`. Fix the sandbox ([Security & approvals](./security.md)) or unset it.

## Next

- [Tutorial: a small agentic team](./tutorial-team.md): the mechanics of leaders, workers, branches and verification in a one-hour setup.
- [Sub-agents](./sub-agents.md): where teams are searched for, and how the leader's flags apply.
- [Security & approvals](./security.md): approval modes and the shell sandbox.
