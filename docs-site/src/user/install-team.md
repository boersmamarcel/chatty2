# Install a team from the marketplace

**When to read this:** Someone published an agent team for a job you have — a payment run, an architecture review, a data analysis — and you want to run it on your machine without copying spec files by hand.

A **team** on Hive is a published agent spec (its **leader**) plus the specs it delegates to that the same publisher published (its **members**). Each spec is signed by its publisher, and every plugin a spec uses is **locked**: pinned at publish time to one exact version and the SHA-256 of its bytes. Installing a team gets you exactly what was published, or nothing.

## Find a team

1. **Settings → Extensions**, and sign in to Hive under **Hive Account** (installing needs your account; browsing does not).
2. Under **Browse Marketplace**, switch to **Teams** and search by name, publisher or task (an empty search lists every team).

Each team shows:

| Field | What it is |
|-------|------------|
| Name, description | The leader spec's |
| **by** · **v** · **installs** | The publisher, the latest version, and how many people installed it |
| **Team:** | The members that are installed with it, at their versions |
| **Plugins (locked):** | Every plugin its specs use, at the locked version; **Paid** marks one that costs credits |
| **Try:** | The publisher's example prompt |

## Install it

Click **Install team**. Chatty then:

1. fetches the leader's spec and each member's;
2. checks each spec's signature chain against the registry root keys built into Chatty, and that it is the name and version asked for. A spec that was changed after signing, or signed by a key the registry never certified, stops the install;
3. downloads each locked plugin through the same signed install path as **Install** on the Plugins tab, and checks that its bytes have the SHA-256 the lock pins;
4. writes the specs to your global agents folder (`~/.local/share/chatty/agents/` on Linux, `~/Library/Application Support/chatty/agents/` on macOS);
5. adds the leader to your roster.

Nothing is written until every check has passed. A confirmation then names the agents that were installed.

If a spec of the same name is already in your agents folder (your own, or part of another installed team), the install stops and tells you which file is in the way. Rename or remove it first.

**Paid plugins.** A team may use paid plugins. They install like free ones, but each call needs credits and the plugin's **billing** grant under **Settings → Plugins**, the same as a paid plugin you install yourself. The confirmation names each one.

**Paid teams** (a fee per run) are not available yet. Such a team shows **Paid team · not available yet**, and installing it is refused.

## Run it

Start a chat and type `/agent `. The leader is in the picker. Pick it and give it the task, e.g. `/agent payments-lead Check vendors.csv before Friday's run`. The leader delegates to its members as its spec says, and **Settings → Agents** lists every installed spec as **served**.

If you declared a roster yourself (`virtual_agents` in the module settings), Chatty adds the leader and the agents it delegates to there too, and removes them again when you uninstall.

## Update or uninstall

- **Update team** appears when a newer version is published. It runs the same checks and replaces the team's specs and plugins with the new version's.
- **Uninstall** removes the team's specs, the roster entries it added, and each plugin it installed that no other installed team still uses. A plugin you had installed yourself before the team stays.

## From the terminal

```bash
chatty-tui --install-team payments-lead          # the latest version
chatty-tui --install-team payments-lead@1.0.0    # an exact version
```

This does the same checks with the Hive sign-in the desktop saved, prints what it installed, and exits. Run the team in the TUI with `chatty-tui --broker`, then `/agent payments-lead <task>`.
