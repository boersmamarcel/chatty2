# chatty-playbook

Research crate: ACE-style playbook memory for evolving agent instructions.

## Role in the system

```mermaid
flowchart LR
  Runs[Agent runs] --> Playbook[chatty-playbook]
  Playbook --> Memory[Playbook store]
  Memory --> Preamble[Updated system preamble]
  Preamble --> Agent[chatty-core AgentFactory]
```

Part of **Self-improving chatty2** — optimizes real preambles inside Chatty, not a
parallel lab.

## Boundaries

Reserved functions listed in [`RESERVED.md`](../../RESERVED.md). Agents scaffold tests
only around `todo!("human: …")` markers.

## Related docs

- [crate-promises-chatty-playbook](https://github.com/boersmamarcel/chatty2/blob/5cd0874909dd36318f542bce39e7249b79800e56/docs/archive/research/crate-promises-chatty-playbook.md)
