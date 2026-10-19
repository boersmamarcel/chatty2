# chatty-flow

Research crate: AFlow workflow representation and IR for searchable agent topologies.

## Role in the system

```mermaid
flowchart LR
  Flow[chatty-flow IR] --> Search[Topology search]
  Search --> Optimize[chatty-optimize]
  Optimize --> Agent[chatty-core runtime]
```

Defines `WorkflowRepr` and intermediate representation used by offline optimizers.

## Boundaries

See [`RESERVED.md`](../../RESERVED.md) for human-only entry points.

## Related docs

- [crate-promises-chatty-flow](https://github.com/boersmamarcel/chatty2/blob/5cd0874909dd36318f542bce39e7249b79800e56/docs/archive/research/crate-promises-chatty-flow.md)
