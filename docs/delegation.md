# Delegation by policy

**When to read this:** You are changing who may call whom (`swarm.delegates_to`,
`swarm.callers`), the call chain, run budgets, or how approvals and questions
travel from a worker back to a person (PL-S2, the DP-* issues).

An agent may call the agents its spec lists, nothing else. The decision is made
by the broker, before anything is spawned, and never by the worker that asks.
The field reference is [agents-and-specs.md](agents-and-specs.md); this page is
the rules in one place.

## Who may call whom

- **Rights come from the spec, not the profile.** `swarm.delegates_to` (names or
  `*` globs) is what gives an agent `list_agents` and `invoke_agent`; a tool
  profile neither adds nor removes it. Empty means no agent tools.
- `swarm.callers`, when set, narrows the other side: only those specs may call
  this one. `exposed = false` hides a spec from listings.
- `chatty_core::services::delegation_policy` implements the check
  (`may_call(caller, callee)` for a node, `root_may_call(callee)` for the root).
  A plain root (a bare `chatty-tui`, the desktop conversation) may call anyone;
  its own tools decide whether it delegates at all. A `--team` leader or
  `--agent <spec>` root is held to its spec.
- A broker given no policy consults the named `LocalPermissive`; the check is
  never skipped.

## The call chain

- A `CallChain` (`chatty-fabric/src/delegation.rs`) is the list of spec names a
  call went through, root first, plus a `root_task_id` minted when the root
  starts a call. Every run under that root call shares the `root_task_id`, on
  every local hop (pinned by `root_task_id_is_stable_across_hops`).
- **The broker stamps the chain.** It reads the caller's chain from its own
  task table (the run the `agent.invoke` names) and extends it with the callee.
  Nothing in the request can set or widen it: the chain is not a wire field, and
  a frame that smuggles one in (`metadata.chatty.call`) does not decode. No
  bearer or token travels with a local call; a worker never holds a credential.
- `CallChain::extend` refuses a **cycle** (the callee is already on the chain)
  and a call deeper than **`MAX_DEPTH` = 4** (the root's callee is depth 1).
  The cycle is checked first.
- A refusal is `CallError::Delegation(Refusal)`, written to the edge log and
  shown to the calling model as `Error: invoke_agent: cycle: …`,
  `too_deep: depth 5 > max 4` or `not_listed: a may not call c`.

## Budgets

- Each spec may declare `[budget]` (turns, time, `cap_usd`). A callee runs under
  the tighter of its own budget and what its caller has left
  (`CallChain::budget`): a caller can only lower what it passes down.
- `invoke_agent` sends what is left with every call
  (`InvokeAgentParams.remaining`). A used-up limit is refused before spawn as
  `budget_spent: turns|seconds|usd`.
- Dollars are enforced in-process by `LocalSpendGate`; see
  [token-tracking.md](token-tracking.md#the-local-spend-gate-dp-3).
- The broker stops a callee still running past its deadline plus
  `deadline_grace` and returns a failed result.

## Approvals and questions

- `human.approve` (a shell or write approval) is answered only by the root. The
  broker stamps the asker from the node's admitted name and chain, never from
  the frame, and never sends the request to another worker.
- `human.ask` (a question) climbs the caller chain: each caller may answer or
  `escalate`, and the first answer found goes back down. The wire shapes are in
  [agents-and-specs.md](agents-and-specs.md).

## Calls that leave the machine

Cross-host authority is a broker-held ledger hold (ADR-0022), not a token on
the A2A wire. `invoke_agent` refuses remote delegations that fail the same
budget check. That work is tracked separately (CX-*) and is not described here.
