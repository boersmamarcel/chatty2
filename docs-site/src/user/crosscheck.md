# Crosscheck: several attempts, one judged answer

**When to read this:** One answer from the model is too often plausibly wrong for the job — a figure from your data, a list, a fix whose tests must pass — and you would rather spend more tokens than check it yourself.

**Crosscheck** is a built-in team: several independent attempts at the same task, and a judge picks the strongest. It is free and runs on your own model. It is **experimental**: it ships so you can use it, and it becomes a documented default only once a measured result supports it (see [What it is worth](#what-it-is-worth)).

## What it does

1. **Three attempts at once.** The leader hands your task, word for word, to three solvers. Each runs as its own agent with its own context: none sees another's work. They approach the task differently — one directly, one after writing a plan, one after deciding how it will check its answer — so their mistakes are less alike. They run at the same time when your model server takes several requests at once (see [Speed](#speed)).
2. **A check, when there is one.** If the team declares a verification command (a test suite, a checker script), Chatty runs it on each attempt's own copy of the project. An attempt that passes wins; the judge is not asked.
3. **Otherwise a judge picks.** The judge reads every attempt's final answer and its trace — the commands and code it ran and what they printed — and names one. It never writes an answer of its own: what you get is the chosen attempt's answer, character for character. When all attempts give the same answer, no judge is needed. The judge never thinks out loud (thinking is off for it), which was both faster and more accurate in our tests.
4. **The cost is on screen.** The reply starts with a line like `3 attempts + judge, 41,200 tokens (≈3.4× a single run)`, then the answer, how it was chosen, and each attempt's answer.

Two versions ship:

| Team | Id | Use it for |
|------|----|-----------|
| Crosscheck | `crosscheck` | Any task with one right answer you can describe: a figure, a name, a short fix. |
| Crosscheck: Data | `crosscheck-data` | Questions about data files in your workspace. Its three attempts are data analysts that read the documentation that comes with the data first and compute with code. |

## Run it

From the terminal:

```bash
chatty-tui --team crosscheck-data -m "What was the average transaction value in March 2023, in EUR, rounded to 2 decimals?"
```

In the desktop app there is no team switch yet: name the team's agents in `virtual_agents` in `module_settings.json`, restart, and send `/agent crosscheck-lead <task>` (or `/agent crosscheck-data-lead <task>`):

```json
"virtual_agents": ["local-agent", "crosscheck-lead", "crosscheck-solver-direct",
  "crosscheck-solver-plan", "crosscheck-solver-verify", "crosscheck-judge", "crosscheck-writer"]
```

The team's members are not offered to you one by one: they work only for their leader.

## What it costs

About three and a half single runs: three attempts, plus a judge when they disagree. The banner shows the real figure for each run, measured against the attempts themselves. It is opt-in per conversation and never the default, because most tasks do not need it.

**Use it when** a wrong answer is expensive and the task has one right answer: a number from a dataset, a yes or no, a fix with tests. **Skip it when** the work is long and sequential (a refactor, a report built step by step): independent attempts do not help there, and coordination teams in general have not made sequential work more accurate.

## Speed

The attempts run at once only if your model endpoint allows several requests at a time. Chatty sends one request at a time per endpoint unless you raise its budget: set `num_parallel` in the provider's `extra_config`, or `default_endpoint_budget` (or a per-endpoint entry in `endpoint_budgets`) in `module_settings.json`, to 3 or more. With a budget of one, the attempts queue and the run takes about three times as long.

## What it is worth

Measured on DABstep, a benchmark of data questions, before release: the plan and the gate are in [the pre-registration](https://github.com/boersmamarcel/chatty2/blob/main/docs/research/crosscheck-prereg.md); the result note will be listed here when the run finishes. Until then, treat Crosscheck as experimental.
