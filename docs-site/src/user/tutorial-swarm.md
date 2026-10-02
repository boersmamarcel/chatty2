# Tutorial: from one agent to a team

**When to read this:** You have Chatty installed and a model connected, and you want to see a team of agents do real work: a lead that plans and delegates, specialists that each do one job, and you in charge of anything that touches your machine. Ten minutes of reading, plus a few minutes of the agents working.

## What you will do

You give a sales export to a three-agent team and ask it a real question: *revenue fell in August. Why?* A lead breaks the question down, an analyst answers it with SQL queries, and a reviewer works out the key numbers again before you see the report. On the way you will:

1. set up the example;
2. look at the agents Chatty ships;
3. start the team with one command;
4. watch the team work, live;
5. approve the file a worker wants to write;
6. read the result, and who spent what, per model;
7. move one agent to a second, smaller model;
8. build a two-agent team of your own.

> [!NOTE]
> Agent teams are supported and **experimental**. The shipped teams show what Chatty can do with a team. They are not a claim that a team beats a single agent; that research comes later. Try them, and compare with a single agent on your own tasks.

## Prerequisites

- The Chatty desktop app, on Linux or macOS ([Getting started](./getting-started.md)). Teams need the local agent broker, which is not available on Windows yet.
- One model that is good at tool calls. The screenshots use `Qwen3.8-27B-INT4` served by a local vLLM, added as an OpenAI-compatible provider ([Providers & models](./providers-and-models.md)). A hosted model works just as well.
- For step 7, a second model, for example `qwen3:4b` in [Ollama](https://ollama.com).
- A workspace folder. This page uses `~/sales`.

## 1. Set up

**Add the team.** Its agents ship with Chatty but are experimental, so they are not on your roster until you name them. Quit Chatty, then write `module_settings.json` (create the file if it is not there) and start Chatty again:

- macOS: `~/Library/Application Support/chatty/module_settings.json`
- Linux: `~/.config/chatty/module_settings.json` (or `$XDG_CONFIG_HOME/chatty/module_settings.json`)
- Windows: `%APPDATA%\chatty\module_settings.json` — teams need the local agent broker, which is not available on Windows yet (see Prerequisites)

```json
{
  "virtual_agents": ["local-agent", "data-lead", "data-analyst", "reviewer"]
}
```

A file at the wrong path is simply never read: Chatty starts with the default roster (`local-agent` and your own exposed specs) instead, silently. If `/agent data-lead …` below ends up answering as `local-agent` instead, re-check this path first — see [Where Chatty stores data](./advanced.md#where-chatty-stores-data) for every platform's config and data directories.

**Get the sample data.** `orders.csv` has 793 orders from July and August: date, region, channel, product, units, price, discount, promo code and revenue. Something happened in August.

```bash
mkdir -p ~/sales && cd ~/sales
curl -LO https://boersmamarcel.github.io/chatty2/assets/samples/orders.csv
```

**Point Chatty at the folder.** In **Settings → Code Execution**, turn on **Enable Code Execution**, set the workspace to `~/sales` and set the approval mode to **Always ask**. Reading files and querying data never ask. Running a command or writing a file does, including when an agent deep inside the team wants to do it. The default mode, **Auto-approve sandboxed**, lets sandboxed commands and writes inside the workspace through without asking. With it, this run asks you nothing.

## 2. Meet the agents

Open **Settings → Agents**. Every local agent is a *spec*: a short TOML file that gives the agent a name, a role, a tool profile, plugins, the agents it may call, and a budget. The presets ship with Chatty. Your own specs go in `<workspace>/.chatty/agents/`.

![Settings → Agents lists every agent spec, preset or your own](../assets/screenshots/swarm-01-settings-agents.png)

This tutorial uses three of the presets. Together they make up the **data-analysis** team:

| Agent | Job | Can use |
|---|---|---|
| `data-lead` | Breaks the question into parts a query can answer, delegates them, writes the report. It computes nothing itself. | Reading files; calling its two workers and no one else |
| `data-analyst` | Answers each part with SQL queries over the file and reports the exact numbers. At the end it saves the report. | Querying data and writing files; no shell |
| `reviewer` | Checks the report against the data. It works out key numbers again itself, edits nothing, and answers `APPROVE` or `REQUEST_CHANGES`. | Reading files and querying data; no shell, no writes |

Each line on the page also shows the agent's model (none of the presets names one, so they run your default), its tool profile, which agents it may call, and any plugins it runs with. **Settings → Plugins** lists the WASM plugins an agent can load, with what each asks for and which agents use it. This team needs none; [Give an agent a plugin](../dev/start/tutorial-benford-agent.md) shows how to add one.

## 3. Ask the team

Start a new chat and type:

```text
/agent data-lead Revenue in orders.csv fell in August. Find out why.
```

`/agent <name>` gives the turn straight to that agent: your own model is not asked anything. The lead gets the question and starts delegating.

## 4. Watch the team work

The turn opens into a live **swarm tree**. Each line is one agent: its model, or the tool it is running right now, then its status and the tokens it has used so far. Workers get numbered names (`data-analyst-0`), so you can tell a second call to the same role from the first.

![The swarm tree while the team works](../assets/screenshots/swarm-02-live-tree.png)

Click a line to open that agent's transcript in a side sheet: its spec, model, turns, spend per model, and every tool call it made with the input and result. The breadcrumb at the top (`data-lead › data-analyst-0`) takes you back up the tree.

![Drilling into the analyst: the queries it ran and what they returned](../assets/screenshots/swarm-03-drill-in.png)

## 5. Approve the report

When the reviewer has approved, the lead asks `data-analyst` to save the report as `report.md`. Writing a file is a change to your workspace, so it waits for you. The request travels up the chain, from worker to lead to you, and the card tells you who is asking and from where:

![The analyst, two levels down, asks to write report.md](../assets/screenshots/swarm-04-approval.png)

Click **Approve** (or press Alt+Y). **Deny** (Alt+Shift+N) sends the worker an error instead, and nothing is written. The decision goes back down the same chain, so only the worker that asked writes the file.

The analyst's queries never asked: under **Always ask**, only commands and writes come to you. A worker that wants to run a command asks the same way, with the command on the card.

## 6. Read the result and the bill

After a few minutes the lead answers with the reviewed report. On the sample data, a good answer has three parts:

- **The size of the drop:** August revenue is 43,886 against 53,251 in July, 17.6% down.
- **The biggest driver:** fewer `Pro` orders (132 down to 91) at an unchanged price, most of them in the EU. EU `Pro` alone went from 47 orders to 14, about 6,600 or 70% of the drop.
- **The second driver:** the `SUMMER30` code, 30% off `Starter` bought online from 1 August. It gave away about 1,760 in discounts, and `Starter` volume did not grow.

The data shows how much and where, not why fewer people bought `Pro`. A good report says so and suggests what to check, rather than guessing. Reports differ from run to run in how they slice the numbers; in our runs every one found both drivers.

![The reviewed report](../assets/screenshots/swarm-05-report.png)

Scroll back up to the tree. When the run has finished, every line shows what that agent used, and the header shows the total for the whole team. The side sheet splits an agent's spend **by model**. A model with a price in **Settings → Models** shows in dollars; a local model like the one here shows in tokens. The conversation's cost in the sidebar includes everything its agents spent.

![The finished tree: each agent's model and tokens, and the team total](../assets/screenshots/swarm-06-bill.png)

## 7. Mix models

Every preset names no model, so the whole team runs on your default. To change one agent, copy its spec into your workspace and add a `model` line. A spec in `<workspace>/.chatty/agents/` replaces the preset with the same name.

The reviewer's job is narrow: query the data, compare, give a verdict. A small local model can do it. Add `qwen3:4b` under **Settings → Models** (Ollama provider), then create `~/sales/.chatty/agents/reviewer.toml`:

```toml
[agent]
name = "reviewer"
model = "qwen3:4b"
description = "Checks a report against its sources and answers APPROVE or REQUEST_CHANGES"
preamble = "Verify, do not trust. You check someone else's report against the sources it rests on, and edit nothing. Re-derive at least two of its key facts yourself from the source (query_data on a data file, read_file on a document); take results a named tool or plugin produced as given. Check that every claim is supported and none is overstated. The first line of your answer is APPROVE or REQUEST_CHANGES, then a numbered list: what you checked, or what must change."

[tools]
profile = "reviewer"
disable = ["shell"]

[swarm]
exposed = true

[budget]
max_agent_turns = 12
```

`model` is matched against your model list the same way as the model selector: by id, by name, or by part of the model identifier. Ask the same question again. The tree now shows two models, and the side sheet bills each agent by model. A small model on a CPU is slow, so expect the reviewer's turn to take longer.

**A cloud model as the lead.** The opposite mix is common too: a strong hosted model plans and writes, and cheaper local models do the legwork. Copy `data-lead` the same way and point it at a hosted model from your list:

```toml
# ~/sales/.chatty/agents/data-lead.toml: the preset, plus the model line
[agent]
name = "data-lead"
model = "claude-sonnet"   # any model in your list: its id, name, or part of its identifier
description = "Experimental team lead (data-analysis): answers a business question about a data file with an analyst and a reviewer"
preamble = """
You lead a data analysis and compute nothing yourself.
1. Turn the question into two to four sub-questions a script can answer: the totals, which segments changed and by how much, and whether volume, price or discount moved in each.
2. Ask data-analyst to answer them, naming the file.
3. Write the report: the answer in one or two sentences, then each finding with its numbers and its share of the change, then what to do next. Every number comes from the analyst, never from you. Where the data shows how much but not why, say so instead of guessing a cause.
4. Send reviewer the report and the file's path. On REQUEST_CHANGES, fix exactly what it lists, asking the analyst again only for a disputed number, and send the report back once.
5. Ask data-analyst to save the reviewed report, exactly as written, as report.md next to the data file.
Your final answer is the reviewed report, with any notes the reviewer still has."""

[tools]
profile = "coordinator"

[swarm]
delegates_to = ["data-analyst", "reviewer"]

[budget]
max_agent_turns = 16
```

A spec is one complete file. It is not a patch on the preset, so a copy keeps every section. Only the lead's tokens are billed at the hosted price, and the swarm tree and the sidebar show the split.

## 8. Build your own two-agent team

A team is just agents that are allowed to call each other. Here is a lead that turns the findings into a memo for your sales director, and a writer it delegates to. Create these two files in `~/sales/.chatty/agents/`:

```toml
# memo-lead.toml
[agent]
name = "memo-lead"
description = "Turns analysis findings into a short memo for a manager"
preamble = "You write nothing yourself. Give memo-writer the findings you were given and ask for memo.md: at most 200 words, the answer first, then one action per finding with who should take it. Check the memo covers every finding, then answer with its text."

[tools]
profile = "coordinator"

[swarm]
delegates_to = ["memo-writer"]

[budget]
max_agent_turns = 6
```

```toml
# memo-writer.toml
[agent]
name = "memo-writer"
description = "Writes short internal memos into a file"
preamble = "You write short, plain internal memos. Write exactly the file you are asked for, using only the facts you were given, then reply with its text."

[tools]
profile = "coder"

[budget]
max_agent_turns = 4
```

- `delegates_to` is the only thing that lets `memo-lead` call another agent. It can call `memo-writer` and no one else.
- The `coordinator` profile can read and delegate, but it cannot write or run commands. The `coder` profile can write files. Each file it writes still asks you first.
- `[budget]` caps each agent's model turns, so a confused agent stops instead of running on.

Add `"memo-lead"` and `"memo-writer"` to `virtual_agents` in `module_settings.json` and restart Chatty: a roster you name is exactly those agents. Open **Settings → Agents**; both agents appear. Then paste the findings from step 6 into:

```text
/agent memo-lead Write a memo to the sales director. Findings: <paste the answer and findings from the report>
```

When `memo-writer` wants to create `memo.md`, you get the same kind of card as in step 5. A minute later the memo is in `~/sales/memo.md`: the answer first, then one action per finding, each with an owner.

## Verify

- Step 6's report puts the drop at 17.6%, names EU `Pro` as the biggest driver (about 70%) and the `SUMMER30` discount on online `Starter` as the second (about 27%).
- The swarm tree shows `data-lead` with `data-analyst` and `reviewer` below it. After step 7, `reviewer` has a different model from the rest.
- `~/sales/report.md` exists after step 5, and `~/sales/memo.md` after step 8; you approved both writes.

## Common issues

| You see | Why | Fix |
|---|---|---|
| `data-lead` shows **Preset · not in the roster**, and `/agent data-lead …` goes to the default agent | The team's agents are not in `virtual_agents` | Step 1: name all three in `module_settings.json`, then restart |
| No approval card at all | The approval mode is **Auto-approve sandboxed** (the default), which lets writes inside the workspace through | Switch to **Always ask** in Settings → Code Execution to see each write and command first |
| The reviewer appears twice in the tree | It asked for changes, and the lead fixed the report and sent it back | This is the review loop working. The lead sends the report back only once. |
| A run takes a long time on a local model | Every agent's turns share one model server, one request at a time | Expect five to ten minutes on a single local GPU. A hosted lead (step 7) makes it faster. |
| The small model's answer is wrong | Some roles need a stronger model | Keep small models on narrow roles like `reviewer`, and the lead and analyst on your strongest model |

## Next

- More teams, all experimental: `research-brief` ships with this release (a folder of documents to a sourced brief, see [Sub-agents](./sub-agents.md#teams)), and `analyst-panel`, where three analysts answer on their own and an adjudicator picks one when they disagree.
- [Tutorial: a small agentic team](./tutorial-team.md): the terminal version, a coding team with its own playbook
- [Security & approvals](./security.md)
