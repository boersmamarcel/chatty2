# Memory & skills

**When to read this:** You want the agent to remember things between conversations, or to reuse a procedure it has worked out once.

## Memory

Memory is on by default. The agent stores facts, preferences, decisions and project conventions as it works — you can also just tell it *remember that we deploy from the `release` branch* — and searches them when earlier context would help. Everything is kept in a local file on your machine ([where](./advanced.md)); nothing is uploaded.

Out of the box, recall is full-text search. **Semantic Search** adds search by meaning, so *how do we ship?* finds the note about the release branch; it needs an embedding provider.

### Settings → Memory

- **Enable Agent Memory** — the master switch.
- **Purge All Memory** — permanently delete every stored memory. This cannot be undone.
- **Enable Semantic Search**, with an **Embedding Provider** (it can differ from your chat model's provider) and an optional **Embedding Model** — leave it empty for the provider's default.
- **Memory Browser** — **Load Memories**, search them, **Refresh**, and **Delete** individual entries.

> [!NOTE]
> Memory is shared across conversations and across the desktop and terminal apps, so a fact the agent learns in one chat is available in the next.

## Skills

A skill is a named, reusable procedure — *deploy-to-staging*, *write-unit-tests* — that the agent can load and follow later. Ask it to save one (*save what you just did as a skill called deploy-to-staging*) and it writes the steps down under that name; on a later task it finds relevant skills through memory search and loads the instructions before starting.

You can also invoke a skill yourself: type `/` in the composer and pick it — skills appear in the picker with a skill badge.

A skill is a folder holding a `SKILL.md` file, in the [Agent Skills](https://agentskills.io) format that Claude Code, Codex, opencode and Cursor also read. The folder name is the skill name; the `description:` line in the file's frontmatter is what the picker shows:

```markdown
---
name: deploy-to-staging
description: Build, tag and push the current branch to staging.
---

1. Run the tests …
```

Chatty looks for skills in two places:

- **Project skills**, next to your code so they travel with the repository: `.agents/skills/<name>/SKILL.md` or `.claude/skills/<name>/SKILL.md`. Chatty searches the conversation's working folder and every folder above it up to the root of the git repository.
- **Global skills**, available in every project: `~/.agents/skills/<name>/SKILL.md` or `~/.claude/skills/<name>/SKILL.md` in your home folder. The path is the same on macOS, Linux and Windows (`%USERPROFILE%\.agents\skills\` on Windows).

Use `.agents/skills/` for new skills. Chatty reads `.claude/skills/` as well, so skills you already have for Claude Code show up without copying them. When two skills share a name, the project skill wins over the global one, and `.agents` wins over `.claude`.

The `/` picker rescans these folders each time you open it, so a skill you add while Chatty is running shows up the next time you type `/`.

## Next

- [Agents & tools](./agents-and-tools.md)
- [Sub-agents](./sub-agents.md)
- [Advanced](./advanced.md)
